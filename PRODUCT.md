# Prismestra product scope

Prismestra coordinates PC RGB, ambient lighting, selected BLE devices, LCDs and compatible home lights in one local scene. Existing Rust canvas/effects/capture/LCD components are reused; OpenRGB SDK is the planned broad PC backend. [Current implementation](README.md), [acceptance gaps](PARITY.md).

## Boundaries

- No fan curves, PWM writes, cooling guardians, cooling recovery or replacement of FanControl. Read-only temperature sources remain in scope.
- Device discovery, binding and activation are separate. Loading a portable profile never authorizes native I/O.
- Portable scenes/assets and local hardware preferences remain separate.
- Each endpoint has one owner; cancellation must drain pending I/O before releasing its resources.
- Windows is the current platform. Other hardware and firmware are supported only when explicitly implemented and tested.

## User experience

The editor must explain what is connected, which scene is active, what is unavailable and how to add or replace a device. A web/JS editor is allowed if it can close while the lightweight engine continues. That lifecycle separation is not yet implemented. No permanent Chromium/Electron engine is required by the product.

Schedules target lighting groups. LCD overlays use logical temperature names, including aggregate RAM and Liquid. Compatibility should distinguish implemented protocol, software verification and physical acceptance.

## Acceptance

A release requires a reproducible build, simple first connection, real RGB/LCD output through the ordinary user flow, profile/assets migration checks, explicit compatibility limits and CPU/RAM measurements against a comparable scene. Passing isolated tests alone is insufficient. [Implementation and remaining gates](PARITY.md).
