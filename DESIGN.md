# Native application direction

This is an Operate surface: the user edits behavior and understands state. Product truth is in PRODUCT.md and PARITY.md. Direction is chosen under the user's explicit delegation; the old OpenRGB UI is behavioral reference, not a visual lock.

Use familiar Windows controls, Segoe UI, system focus/keyboard behavior and DPI-scaled geometry. Quiet off-white surfaces, charcoal text, thin neutral separators, and one restrained warm accent inherited from the lighting context. The actual animated canvas is the visual lead. Status is written in words; colors never carry it alone. Avoid decorative metric tiles, invented icons, gradients/glass and custom imitations of standard inputs.

The first native surface connects the real scene/map/schedule editor on the stand. It must prominently say preview/stand; no “connected” state or real temperature is invented. Visible controls do real work. Other full-product requirements remain in PARITY rather than being presented as functioning buttons.

Layout: compact title/stand notice, conventional navigation, large shared canvas, tightly grouped scene parameters and device table. Separate schedule editing with explicit timezone and readable daily times. Save/open names describe scene/map/schedule scope. Dirty/saved/error feedback is textual and persistent enough to read. No hardware activation action exists in this preview surface.

Scale all fonts, hit targets and spacing with DPI, retain sharp native text, reflow at the minimum supported window size. Use buffered GDI painting for the actual window; snapshot tests print that same window and its native child controls, not a separate illustrative mock. Inspect bounded batches at96/144DPI and narrow/wide sizes, fix once and confirm once. This is an implementation contract, not a claim that UI acceptance already passed.

UI builds embed Common Controls6 visual styles without a new runtime dependency. Client snapshots use an owned borderless off-screen window so explicit test DPI is independent of the host monitor's frame; interactive windows keep their normal frame and per-monitor DPI handling. Save/open stay above the persistent status bar; property rows use26DIP. Evidence: `build.rs`, `resources/ui.manifest`, `src/ui/windows.rs`, `work/ui-layout-stand/accepted-*.out`. These client-only captures do not prove interactive frame resizing or hardware compatibility. The full operator design/onboarding gate remains open.
