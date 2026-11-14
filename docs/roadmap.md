# Product direction

Prismestra combines local PC lighting, supported BLE controllers, LCD content and home lighting in one scene. It reuses OpenRGB, WLED and Home Assistant where they expose the needed operations. Fan and pump control remain outside this product. A supported protocol is not a promise that every device from a vendor works.

## Implemented foundation

The source includes the scene engine and editor, resident tray lifecycle, separate portable profiles and private connections, OpenRGB/WLED/Home Assistant discovery and capability handling, Surplife E1 controller capacity checks, LCD composition, read-only temperature routes and bounded output recovery. See [desktop behavior](desktop-editor.md), [connections](universal-connections.md) and the [compatibility matrix](compatibility.md) for evidence and limits.

The portable Windows package adds separate preview and saved-scene launchers with folder-relative state. It does not bundle or install OpenRGB, configure Windows startup or migrate credentials. See [portable launch](portable-launch.md).

## Next stages

1. **Make setup easier to complete.** Walk through an empty profile, device discovery, layout, save, connection replacement and recovery as complete user scenarios. Reuse existing validators and capabilities. Completion means the supported setup paths are understandable and tested; adding more vendor names is not its measure.
2. **Measure the current product.** Record CPU and memory for the complete required process tree with the editor open and closed. Use fixed scene, output rate, capture stimulus and LCD workload. Only claim an improvement against a matched baseline. Older measurements cannot establish performance for a newer editor or different outputs.
3. **Package for a clean Windows PC.** Verify prerequisites and first launch, preserve user data across updates and document the external backend. Portable packaging tests do not establish clean-machine installation or Windows logon behavior.
4. **Expand through existing standards.** Windows Dynamic Lighting/LampArray is the next planned platform integration, with its own MSIX/deployment work. Additional LCD and BLE families need a specific protocol, fixtures and a visible setup path before hardware support can be claimed. Importing SignalRGB plugins is not part of the current plan.

Work without new hardware should continue using protocol fixtures, owned loopback servers, invalid-input cases and isolated application checks. Physical WLED, Home Assistant lights, a second BLE controller, other strip lengths and unplug/replug remain unverified until equipment is available. Their absence does not block unrelated software delivery. New hardware results must identify the exact device and tested revision.

Public release publication, privileged installation changes and hardware acceptance are separate from preparing and testing a candidate package. A running personal setup is preserved during development.
