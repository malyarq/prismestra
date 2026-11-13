# Reusable device connections

Prismestra reuses backend capabilities instead of treating every physical setup as a new driver. This document describes the implementation, not a claim of universal hardware acceptance.

## Connect a device

Add the appropriate family in Devices, then open Connections for that logical device. Discovery does not activate RGB output. Select a result, save its local binding, then Apply. Applying a discovered strip length updates its canvas geometry in the same runtime transaction. Existing same-sized layouts remain intact. Discovery does not guess the physical arrangement of a strip.

- **OpenRGB:** leave the SDK endpoint empty to use `127.0.0.1:6742`, or supply another explicit server. Find reads the inventory and complete controller geometry. Apply revalidates identity and topology before enabling Direct output. This covers devices supported by the selected OpenRGB backend; it does not implement missing OpenRGB drivers.
- **WLED:** leave the DDP endpoint empty and Find performs a short LAN DNS-SD browse. Alternatively enter an explicit `IP:4048`. Find reads `/json/info` to obtain the configured point count and MAC-based identity. The optional HTTP origin field supports non-default HTTP ports; its IP must match the DDP destination. Before activation the identity and configured count are read again. A changed controller or count requires discovery and Apply; old frames are not silently sent to changed geometry. Legacy explicit DDP-only bindings still load without metadata verification. UDP submission is not confirmation of visible output.
- **Home Assistant:** an empty endpoint finds advertised servers without sending a token. Select a server, correct the URL for HTTPS/reverse proxies if needed, save its key in Windows Credential Manager, then Find again to list lights. Choose one light and save its binding. Initial binding rejects unavailable entities and unknown capabilities. Color-capable lights receive color; brightness/color-temperature-only lights receive brightness; on/off lights receive switching. This path controls one whole light at up to 1 Hz, not individual LEDs.
- **Surplife E1:** the `surplife-e1` family supports an explicitly selected BLE address. Find issues a fixed read-only EA81 capability query, with no configuration or firmware command. Currently only product `0xAA`, LED protocol `11`, one segment and 1..4096 configured points are accepted. Select the result and Apply to adopt the point count. Before output the connection queries capacity again and rejects a mismatch. The 4096 limit bounds host resources; it is not a claim that every controller supports that many physical LEDs. Existing `door_ble` 20-group and 153-point profiles retain their old behavior and are not silently migrated.

A Surplife capability query takes the same in-process endpoint lease as RGB output. An already owned controller must be detached before its discovery query; a rescan never opens a second connection to a running output. External programs still need to relinquish hardware ownership.

## Discovery boundaries

DNS-SD browses `_wled._tcp.local.` and `_home-assistant._tcp.local.` for at most three seconds, with bounded result/event counts and explicit shutdown. It does not scan IP ranges. Advertisements are untrusted hints; TXT URLs are not followed and credentials are never supplied during discovery. IPv6 link-local addresses requiring an interface scope are currently omitted. mDNS can be disabled or blocked between network segments; manual endpoints remain available.

WLED metadata supports upstream default counts through 16384; larger custom builds require a separately verified extension. A changed IP can be rediscovered using the controller's identity. Rediscovery/Apply is currently required for address changes; no claim of automatic roaming across DHCP addresses is made.

## Restore a previously working output

Native activation enables bounded recovery for registered RGB and LCD outputs. Recovery is armed after a successful transport submission. A failed and fully reaped output can reopen after 1, 5 and 15 seconds, at most three attempts per configuration/power epoch. A successful retry does not replenish this budget. Initial connection failures remain explicit errors. Configuration changes, shutdown and suspend cancel pending attempts; capture and temperature readers do not inherit this policy.

Recovery revalidates saved identity/capacity and republishes current scene intent, including Off, instead of replaying an old frame. It does not restart healthy outputs. Errors are not yet classified by protocol: authentication or changed-topology failures also consume the finite budget and then require manual correction. A synchronous start/lease error may remain in global diagnostics after a later successful recovery.

## Evidence

Implementations: [discovery](../src/app/lighting_bindings/discovery.rs), [WLED metadata](../src/transport/wled_info.rs), [DNS-SD](../src/transport/service_discovery.rs), [HA capabilities](../src/devices/home_assistant.rs), [E1 codec](../src/lighting/door.rs), [BLE transport](../src/transport/ble.rs), [recovery](../src/app/runtime/recovery.rs).

Software checks include real Windows HTTP/UDP loopback fixtures, identity/count rejection before UDP output, capability conversion, protocol fragments and fake-driver recovery/lease scenarios. Hardware evidence for the previous 153-point implementation remains [revision-specific](surplife-e1-pixels.md). WLED/HA physical output, a second BLE controller/different physical length and real unplug/reconnect are not confirmed by these software checks.
