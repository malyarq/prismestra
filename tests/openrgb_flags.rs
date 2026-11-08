// SPDX-License-Identifier: GPL-2.0-or-later
use openrgb2::{ClientFlags, ControllerFlags, FlagSet, ModeFlag, ServerFlags, ZoneFlags};

#[test]
fn all_five_flag_sets_preserve_known_bits_and_ignore_future_bits() {
    macro_rules! check {
        ($kind:ty, $known:expr) => {{
            let known: u32 = $known;
            assert_eq!(FlagSet::<$kind>::empty().bits(), 0);
            assert_eq!(FlagSet::<$kind>::default().bits(), 0);
            assert_eq!(FlagSet::<$kind>::full().bits(), known);
            assert_eq!(FlagSet::<$kind>::new_truncated(u32::MAX).bits(), known);
            assert_eq!(FlagSet::<$kind>::new_truncated(1 << 31).bits(), 0);
            assert_eq!(
                FlagSet::<$kind>::new_truncated(known | (1 << 31)).bits(),
                known
            );
            assert!(FlagSet::<$kind>::full().contains(FlagSet::<$kind>::empty()));
        }};
    }
    check!(ModeFlag, (1 << 12) - 1);
    check!(ControllerFlags, 0b111 | (1 << 8));
    check!(ZoneFlags, 0x7f | (0x3f << 12) | (1 << 24));
    check!(ClientFlags, 0x1f | (1 << 16));
    check!(ServerFlags, 0x7f | (1 << 16));
}

#[test]
fn mode_combinations_and_composite_direction_keep_the_same_semantics() {
    let combined = ModeFlag::HasSpeed | ModeFlag::HasBrightness;
    assert_eq!(combined.bits(), 0x11);
    assert!(combined.contains(ModeFlag::HasSpeed));
    assert!(combined.contains(ModeFlag::HasBrightness));
    assert!(!combined.contains(ModeFlag::AutomaticSave));
    let direction = ModeFlag::HasDirectionLR | ModeFlag::HasDirectionUD | ModeFlag::HasDirectionHV;
    assert_eq!(direction, ModeFlag::HasDirection);
    assert_eq!(direction.bits(), 0x0e);
    assert!(direction.contains(ModeFlag::HasDirectionLR));
    assert!(!ModeFlag::HasDirectionLR.contains(ModeFlag::HasDirection));
    assert_eq!((combined & !ModeFlag::HasSpeed).bits(), 0x10);
    for bits in 0..=0xfff {
        assert_eq!(ModeFlag::new_truncated(bits | (1 << 31)).bits(), bits);
    }
}
