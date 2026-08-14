# Compatibility and Hardware

u-forge v0.1.1 is at its best when the machine has enough local compute and
memory to keep worldbuilding, retrieval, and generative AI responsive at the
same time. The knowledge graph itself has much lighter requirements and remains
fully usable when local AI is unavailable.

## Current hardware sweet spot

The ideal all-in-one target today is an AMD **Strix Halo** (Ryzen AI Max) class
system, or a similar AMD device that combines a capable CPU, integrated GPU,
NPU, and a large shared-memory pool. That architecture is a particularly good
fit for running several local inference capabilities without dividing the
machine into small system-RAM and VRAM budgets.

Upcoming **Gorgon Halo** systems are expected to fit the same profile, although
they cannot be treated as validated hardware until the devices and their
Lemonade backends are available for testing.

Strix Halo is a recommendation, not a hard requirement. A conventional desktop
with a discrete GPU remains a supported and useful way to run u-forge.

## The conservative baseline

The shipping defaults are calibrated to an older gaming workstation:

- AMD Ryzen 9 5950X
- 32 GB system RAM
- NVIDIA GeForce RTX 3080 Ti with 12 GB VRAM

That split-memory machine needs more conservative model choices and loading
settings than a high-memory Strix Halo system. The v0.1.1 defaults therefore
favor a dependable out-of-box experience over the largest possible models.
They are a safe starting point, not a ceiling: users with more memory or a
unified-memory APU can select larger models and more ambitious retrieval
features through Lemonade setup and u-forge settings.

Model memory needs vary with the selected model, context size, backend, and the
number of resident capabilities. If setup becomes unstable or the operating
system starts paging heavily, reduce the Assistant model or context size and
avoid keeping optional high-quality embedding models resident.

## Platform compatibility

The official v0.1.1 artifact is an **x86_64 GNU/Linux AppImage** built on Ubuntu
26.04. Arch Linux and CachyOS are the primary development and dogfooding
platforms. A contemporary X11 or Wayland desktop and working graphics drivers
are expected.

On x86_64 GNU/Linux, source builds can download and verify the pinned private
Lemonade runtime used by the AppImage. Other targets can build without that
runtime and connect to a separately managed Lemonade Server, but they are not
part of the v0.1.1 packaged-release compatibility promise. See
[BUILD.md](BUILD.md) for the relevant build options.

## Graceful fallback

Lemonade Server supplies semantic embeddings, reranking, chat, speech-to-text,
and text-to-speech. Availability depends on the models and backends supported
by the local hardware. u-forge discovers the live catalog instead of assuming
a particular accelerator is present.

Without a usable AI backend, worlds, schemas, graph navigation, editing,
import/export, and full-text search continue to work. This is a supported mode,
not a failed installation.
