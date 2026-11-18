# Nested KVM E2E guest

Suite 18 creates an L1 sandbox from this linux/amd64 image. Its QEMU starts the
L2 kernel at `/inner-bzImage` with `/inner.cpio`. The L2 init program writes
its boot ID and a one-second heartbeat to the serial console.

Build with `docker buildx build --platform linux/amd64 --load -t nestedvirt-e2e:local images/nestedvirt`
from the repository root. The Ubuntu base is pinned. The build resolves packages
from Noble at build time, including the kernel from the security pocket. Building
again can pick up newer packages.

To use a prepared image, set `E2E_NESTEDVIRT_IMAGE` to its registry reference
including the published manifest digest, e.g. `registry.example.org/tests/nestedvirt@sha256:<64 hex digits>`.
Both AgentENV nodes must be able to pull it. GitHub Actions reads the repository
variable of the same name. A configured image is required to pull and validate
successfully; the suite never falls back after a failed pull. With the variable
unset, the suite prepares the ordinary Ubuntu guest during the test.
