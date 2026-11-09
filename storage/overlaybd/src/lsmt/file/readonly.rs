use super::helper::*;
use super::types::*;
use anyhow::{bail, ensure, Context, Result};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures_util::{stream, StreamExt};
use std::cmp::min;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use uuid::Uuid;

#[cfg(feature = "io-uring")]
use crate::io::vfile_io::CtxRead;
use crate::io::vfile_io::{read_exact, DirectRead, FileReader};
#[cfg(feature = "io-uring")]
use crate::io::virtual_file::{IoCtx, LocalBoxFuture};
use crate::io::virtual_file::{VirtualFile, VirtualFileWriter};
use crate::lsmt::index::{LogIndex, ReadOnlyIndex, Segment, SegmentMapping};
use storage_util::CompactWriter;

pub struct LSMTReadOnlyFile {
    pub(super) index: Arc<dyn LogIndex>,
    pub(super) index_view: Option<Arc<ReadOnlyIndex>>,
    pub(super) layers: Vec<Arc<dyn VirtualFile>>,
    pub(super) virtual_size: u64,
    max_io_size: AtomicUsize,
    pub(super) uuids: Vec<Uuid>,
    file_type: LSMTFileType,
}

impl LSMTReadOnlyFile {
    pub async fn open(file: Arc<dyn VirtualFile>) -> Result<Self> {
        let file_size = file.size().await?;

        let _header = verify_ht(&file, false, file_size).await?;

        let trailer = verify_ht(&file, true, file_size).await?;

        let mappings = load_index_and_reset_tags(
            &file,
            trailer.index_offset.get(),
            trailer.index_size.get() as usize,
        )
        .await?;

        let index = Arc::new(ReadOnlyIndex::new(mappings));
        let uuid = parse_uuid_field(&trailer.uuid).unwrap_or_else(Uuid::nil);

        Ok(Self {
            index: index.clone(),
            index_view: Some(index),
            layers: vec![file],
            virtual_size: trailer.virtual_size.get(),
            max_io_size: AtomicUsize::new(MAX_IO_SIZE),
            uuids: vec![uuid],
            file_type: LSMTFileType::ReadOnly,
        })
    }

    pub async fn commit(&self, dest_file: Arc<dyn VirtualFile>) -> Result<()> {
        ensure!(
            self.layers.len() <= 1,
            "not supported: commit stacked files"
        );

        let file_size = self.layers[0].size().await?;

        match verify_ht(&self.layers[0], true, file_size).await {
            Ok(trailer) if trailer.is_sealed() => {}
            _ => {
                bail!("Commit a compacted LSMTReadonlyFile is not allowed.");
            }
        }

        let query = Segment::new(0, (self.virtual_size.div_ceil(ALIGNMENT)) as u32);
        let mut mappings = Vec::new();
        self.index.lookup(query, &mut mappings);

        let writer: Arc<dyn CompactWriter> = Arc::new(VirtualFileWriter::new(dest_file));
        let commit_args = CommitArgs::from_writer(writer);
        compact_to(&self.layers, &mappings, self.virtual_size, commit_args).await
    }

    pub async fn commit_preserving_metadata(&self, mut args: CommitArgs) -> Result<()> {
        ensure!(
            self.layers.len() == 1,
            "commit_preserving_metadata requires a sealed single-file overlaybd layer; stacked read-only layers are not supported"
        );

        let file_size = self.layers[0].size().await?;
        let trailer = verify_ht(&self.layers[0], true, file_size).await?;
        ensure!(trailer.is_sealed(), "Commit source must be sealed.");

        args.uuid = parse_uuid_field(&trailer.uuid);
        args.parent_uuid = parse_uuid_field(&trailer.parent_uuid);
        args.user_tag = parse_user_tag_field(&trailer.user_tag);

        let query = Segment::new(0, (self.virtual_size.div_ceil(ALIGNMENT)) as u32);
        let mut mappings = Vec::new();
        self.index.lookup(query, &mut mappings);

        compact_to(&self.layers, &mappings, self.virtual_size, args).await
    }

    pub fn set_max_io_size(&self, size: usize) -> Result<()> {
        ensure!(
            size != 0 && size.is_multiple_of(ALIGNMENT_4K),
            "size {size} must be non-zero and aligned to 4K"
        );
        self.max_io_size.store(size, Ordering::Release);
        Ok(())
    }

    pub fn get_max_io_size(&self) -> usize {
        self.max_io_size.load(Ordering::Acquire)
    }

    pub fn file_type(&self) -> LSMTFileType {
        self.file_type
    }

    pub fn index(&self) -> Arc<dyn LogIndex> {
        self.index.clone()
    }

    pub fn index_view(&self) -> Option<Arc<ReadOnlyIndex>> {
        self.index_view.clone()
    }

    pub fn get_uuid(&self, layer_idx: usize) -> Result<Uuid> {
        self.uuids
            .get(layer_idx)
            .copied()
            .context(format!("layer_idx {layer_idx} out of range"))
    }

    pub fn get_lower_files(&self) -> Vec<Arc<dyn VirtualFile>> {
        self.layers.clone()
    }

    pub fn data_stat(&self) -> DataStat {
        let query = Segment::new(0, (self.virtual_size.div_ceil(ALIGNMENT)) as u32);
        let mut mappings = Vec::new();
        self.index.lookup(query, &mut mappings);
        let valid_blocks: u64 = mappings
            .iter()
            .map(|m| m.length() as u64 * (!m.zeroed as u64))
            .sum();
        let valid = valid_blocks * ALIGNMENT;
        DataStat {
            total_data_size: valid,
            valid_data_size: valid,
        }
    }

    pub fn seek_data(&self, begin: u64, end: u64, segs: &mut Vec<Segment>) -> Result<usize> {
        if end <= begin {
            return Ok(0);
        }
        let begin_blk = begin / ALIGNMENT;
        let end_blk = end.div_ceil(ALIGNMENT);
        if end_blk <= begin_blk {
            return Ok(0);
        }
        let query = Segment::new(begin_blk, (end_blk - begin_blk) as u32);
        let mut mappings = Vec::new();
        self.index.lookup(query, &mut mappings);
        for m in mappings {
            if m.zeroed {
                continue;
            }
            segs.push(Segment::new(m.offset(), m.length()));
        }
        Ok(segs.len())
    }

    pub async fn flatten(&self, dest_file: Arc<dyn VirtualFile>) -> Result<()> {
        let query = Segment::new(0, (self.virtual_size.div_ceil(ALIGNMENT)) as u32);
        let mut mappings = Vec::new();
        self.index.lookup(query, &mut mappings);
        let writer: Arc<dyn CompactWriter> = Arc::new(VirtualFileWriter::new(dest_file));
        let commit_args = CommitArgs::from_writer(writer);
        compact_to(&self.layers, &mappings, self.virtual_size, commit_args).await
    }

    pub(super) fn from_merged_layers(
        layers: Vec<Arc<dyn VirtualFile>>,
        merged: Arc<ReadOnlyIndex>,
        virtual_size: u64,
        uuids: Vec<Uuid>,
        file_type: LSMTFileType,
    ) -> Self {
        Self {
            index: merged.clone(),
            index_view: Some(merged),
            layers,
            virtual_size,
            max_io_size: AtomicUsize::new(MAX_IO_SIZE),
            uuids,
            file_type,
        }
    }

    /// Core read logic that reads directly into the caller-provided buffer,
    /// avoiding intermediate allocations.
    ///
    /// The per-layer read is provided by `reader` so the same body
    /// can serve both the regular `read_at_into` path (which calls
    /// `layer.read_at_into(...)`, yielding a `Send` future) and the
    /// ctx-aware path (which calls `layer.read_at_into_with_ctx(ctx, ...)`,
    /// yielding a `!Send` future). Send-ness of the surrounding future is
    /// monomorphised from `F` — the `Send` callers keep their `Send` futures.
    async fn read_internal_into_generic<R: FileReader>(
        &self,
        reader: &R,
        offset: u64,
        dst: &mut [u8],
    ) -> Result<usize> {
        check_alignment(offset, dst.len())?;

        if offset >= self.virtual_size {
            return Ok(0);
        }
        let read_len = std::cmp::min(dst.len() as u64, self.virtual_size - offset) as usize;
        if read_len == 0 {
            return Ok(0);
        }

        let buf = &mut dst[..read_len];
        let mut remaining = read_len;
        let mut curr_offset = offset;
        let mut buf_pos = 0;
        let max_io_size = self.max_io_size.load(Ordering::Acquire);

        while remaining > 0 {
            let step = min(remaining, max_io_size);

            let begin_blk = curr_offset / ALIGNMENT;
            let count_blk = (step as u64).div_ceil(ALIGNMENT);
            let query_segment = Segment::new(begin_blk, count_blk as u32);

            let mut mappings: Vec<SegmentMapping> = Vec::new();
            self.index.lookup(query_segment, &mut mappings);

            let parallel = mappings.len() > 1;
            let mut reads = Vec::new();
            let mut tail = &mut buf[buf_pos..buf_pos + step];
            let mut consumed = 0;
            for m in mappings {
                let start = ((m.offset() - begin_blk) * ALIGNMENT) as usize;
                if start >= step {
                    break;
                }
                let (hole, rest) = tail.split_at_mut(start - consumed);
                hole.fill(0);
                let length = min((u64::from(m.length()) * ALIGNMENT) as usize, rest.len());
                let (destination, rest) = rest.split_at_mut(length);
                tail = rest;
                consumed = start + length;
                if m.zeroed {
                    destination.fill(0);
                } else {
                    let layer = self
                        .layers
                        .get(m.tag as usize)
                        .with_context(|| format!("Layer index {} out of range", m.tag))?;
                    let read =
                        read_exact(reader, layer.as_ref(), m.moffset * ALIGNMENT, destination);
                    if parallel {
                        reads.push(read);
                    } else {
                        read.await?;
                    }
                }
            }
            tail.fill(0);

            if !reads.is_empty() {
                // Disjoint destinations let independent mappings overlap IO.
                // Drain every borrowed-buffer read before propagating an error.
                let results = stream::iter(reads)
                    .buffer_unordered(32)
                    .collect::<Vec<_>>()
                    .await;
                for result in results {
                    result?;
                }
            }

            remaining -= step;
            curr_offset += step as u64;
            buf_pos += step;
        }

        Ok(read_len)
    }

    async fn read_internal_into(&self, offset: u64, dst: &mut [u8]) -> Result<usize> {
        self.read_internal_into_generic(&DirectRead, offset, dst)
            .await
    }

    #[cfg(feature = "io-uring")]
    async fn read_internal_into_with_ctx<'a>(
        &'a self,
        ctx: IoCtx<'a>,
        offset: u64,
        dst: &'a mut [u8],
    ) -> Result<usize> {
        self.read_internal_into_generic(&CtxRead { ctx }, offset, dst)
            .await
    }
}

#[async_trait]
impl VirtualFile for LSMTReadOnlyFile {
    async fn read_at(&self, offset: u64, len: usize) -> Result<Bytes> {
        if len == 0 || offset >= self.virtual_size {
            return Ok(Bytes::new());
        }
        let read_len = std::cmp::min(len as u64, self.virtual_size - offset) as usize;
        // NOTE: zeroed() means hole regions will be zeroed twice (once here, once
        // in read_internal_into). This is acceptable because read_at is not the
        // hot path in ublk — that uses read_at_into which avoids this allocation
        // entirely.
        let mut buffer = BytesMut::zeroed(read_len);
        let n = self.read_internal_into(offset, &mut buffer).await?;
        buffer.truncate(n);
        Ok(buffer.freeze())
    }

    async fn read_at_into(&self, offset: u64, dst: &mut [u8]) -> Result<usize> {
        if dst.is_empty() {
            return Ok(0);
        }
        self.read_internal_into(offset, dst).await
    }

    async fn write_at(&self, _offset: u64, _data: &[u8]) -> Result<usize> {
        bail!("File is read-only")
    }

    async fn size(&self) -> Result<u64> {
        Ok(self.virtual_size)
    }

    #[cfg(feature = "io-uring")]
    fn read_at_with_ctx<'a>(
        &'a self,
        ctx: IoCtx<'a>,
        offset: u64,
        len: usize,
    ) -> LocalBoxFuture<'a, Result<Bytes>> {
        Box::pin(async move {
            if len == 0 || offset >= self.virtual_size {
                return Ok(Bytes::new());
            }
            let read_len = std::cmp::min(len as u64, self.virtual_size - offset) as usize;
            let mut buffer = BytesMut::zeroed(read_len);
            let n = self
                .read_internal_into_with_ctx(ctx, offset, &mut buffer)
                .await?;
            buffer.truncate(n);
            Ok(buffer.freeze())
        })
    }

    #[cfg(feature = "io-uring")]
    fn read_at_into_with_ctx<'a>(
        &'a self,
        ctx: IoCtx<'a>,
        offset: u64,
        dst: &'a mut [u8],
    ) -> LocalBoxFuture<'a, Result<usize>> {
        Box::pin(async move {
            if dst.is_empty() {
                return Ok(0);
            }
            self.read_internal_into_with_ctx(ctx, offset, dst).await
        })
    }
}
