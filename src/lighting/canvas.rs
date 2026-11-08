// SPDX-License-Identifier: GPL-2.0-or-later
use super::color::Rgb;
/// Explicit routes permit different equipment, without embedding hardware identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub source: usize,
    pub output: usize,
    pub led: usize,
}
/// Validate every route before changing any destination. Duplicate writes are rejected.
pub fn route_frame(
    source: &[Rgb],
    outputs: &mut [Vec<Rgb>],
    routes: &[Route],
) -> Result<(), &'static str> {
    let mut destinations = std::collections::HashSet::new();
    for r in routes {
        if r.source >= source.len() || outputs.get(r.output).is_none_or(|v| r.led >= v.len()) {
            return Err("route out of bounds");
        };
        if !destinations.insert((r.output, r.led)) {
            return Err("duplicate destination");
        }
    }
    for r in routes {
        outputs[r.output][r.led] = source[r.source];
    }
    Ok(())
}
#[derive(Default)]
pub struct DoorCanvas {
    pub received: u64,
    pub revision: u64,
    colors: [Rgb; 20],
}
impl DoorCanvas {
    pub fn capture(&mut self, colors: [Rgb; 20]) {
        self.received += 1;
        if self.revision == 0 || self.colors != colors {
            self.colors = colors;
            self.revision += 1;
        }
    }
    pub fn frame(&self) -> Option<(&[Rgb; 20], u64)> {
        (self.revision != 0).then_some((&self.colors, self.revision))
    }
}
#[test]
fn atomic_routes() {
    let mut out = vec![vec![Rgb::new(1, 2, 3)]];
    let before = out.clone();
    assert!(
        route_frame(
            &[Rgb::default()],
            &mut out,
            &[
                Route {
                    source: 0,
                    output: 0,
                    led: 0
                },
                Route {
                    source: 1,
                    output: 0,
                    led: 0
                }
            ]
        )
        .is_err()
    );
    assert_eq!(out, before);
    route_frame(
        &[Rgb::default()],
        &mut out,
        &[Route {
            source: 0,
            output: 0,
            led: 0,
        }],
    )
    .unwrap();
    assert_eq!(out[0][0], Rgb::default());
}
#[test]
fn revisions() {
    let mut c = DoorCanvas::default();
    assert!(c.frame().is_none());
    c.capture([Rgb::default(); 20]);
    c.capture([Rgb::default(); 20]);
    assert_eq!(c.received, 2);
    assert_eq!(c.frame().unwrap().1, 1);
}
