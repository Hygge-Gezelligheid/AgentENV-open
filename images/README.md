# Guest images

Images that run inside AgentENV sandboxes, as opposed to the host-side server,
gateway, and scheduler images under [`deploy/docker`](../deploy/docker).

| Directory | Image |
|---|---|
| [`tools`](tools) | The envd tools drive attached to every guest as `/dev/vda`. |
| [`compose`](compose) | The Compose guest image used by `aenv compose up`, including the source of its containerd snapshotter in [`compose/plain-snapshotter`](compose/plain-snapshotter). |
| [`nestedvirt`](nestedvirt) | Optional amd64 fixture for the nested KVM snapshot E2E, with QEMU, cpuid, and a bootable L2 kernel and initramfs. |
