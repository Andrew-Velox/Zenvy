//! Small line icons drawn with the painter (1.75 stroke on a 24 grid, like the Android app's vector icons), so the window
//! needs no icon font and no image files.

use eframe::egui::{self, epaint::StrokeKind, Color32, CornerRadius, Pos2, Rect, Shape, Stroke};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Check,
    Phone,
    Screen,
    Apps,
    Speaker,
    Clipboard,
    File,
    Power,
    Unlock,
    Bolt,
    WifiOff,
    Shield,
}

fn arc(cx: f32, cy: f32, r: f32, from_deg: f32, to_deg: f32) -> Vec<(f32, f32)> {
    let n = 14;
    (0..=n)
        .map(|i| {
            let a = (from_deg + (to_deg - from_deg) * i as f32 / n as f32).to_radians();
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

pub fn paint(painter: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width() / 24.0;
    let w = (1.75 * s).max(1.3);
    let st = Stroke::new(w, color);
    let p = |x: f32, y: f32| Pos2::new(rect.min.x + x * s, rect.min.y + y * s);
    let line = |pts: &[(f32, f32)]| {
        painter.add(Shape::line(pts.iter().map(|&(x, y)| p(x, y)).collect(), st))
    };
    let closed = |pts: &[(f32, f32)]| {
        painter.add(Shape::closed_line(
            pts.iter().map(|&(x, y)| p(x, y)).collect(),
            st,
        ))
    };
    let rrect = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        painter.rect_stroke(
            Rect::from_min_max(p(x0, y0), p(x1, y1)),
            CornerRadius::same((r * s) as u8),
            st,
            StrokeKind::Middle,
        )
    };
    match icon {
        Icon::Check => {
            line(&[(4.5, 12.5), (9.5, 17.5), (19.5, 6.5)]);
        }
        Icon::Phone => {
            rrect(7.0, 2.5, 17.0, 21.5, 2.5);
            line(&[(10.5, 18.5), (13.5, 18.5)]);
        }
        Icon::Screen => {
            rrect(3.0, 4.0, 21.0, 16.0, 2.0);
            line(&[(12.0, 16.0), (12.0, 20.0)]);
            line(&[(8.0, 20.0), (16.0, 20.0)]);
        }
        Icon::Apps => {
            rrect(4.0, 4.0, 10.5, 10.5, 1.8);
            rrect(13.5, 4.0, 20.0, 10.5, 1.8);
            rrect(4.0, 13.5, 10.5, 20.0, 1.8);
            rrect(13.5, 13.5, 20.0, 20.0, 1.8);
        }
        Icon::Speaker => {
            closed(&[
                (3.5, 9.5),
                (7.5, 9.5),
                (12.0, 5.5),
                (12.0, 18.5),
                (7.5, 14.5),
                (3.5, 14.5),
            ]);
            line(&arc(12.0, 12.0, 4.5, -45.0, 45.0));
            line(&arc(12.0, 12.0, 8.0, -45.0, 45.0));
        }
        Icon::Clipboard => {
            rrect(5.5, 4.0, 18.5, 21.0, 2.0);
            rrect(9.0, 2.0, 15.0, 6.0, 1.2);
            line(&[(9.0, 11.5), (15.0, 11.5)]);
            line(&[(9.0, 15.5), (15.0, 15.5)]);
        }
        Icon::File => {
            line(&[
                (14.0, 3.0),
                (6.0, 3.0),
                (6.0, 21.0),
                (19.0, 21.0),
                (19.0, 8.0),
                (14.0, 3.0),
                (14.0, 8.0),
                (19.0, 8.0),
            ]);
        }
        Icon::Power => {
            line(&arc(12.0, 13.0, 7.5, -50.0, 230.0));
            line(&[(12.0, 3.5), (12.0, 11.5)]);
        }
        Icon::Unlock => {
            rrect(5.0, 11.0, 19.0, 21.0, 2.0);
            let mut sh = vec![(8.0, 11.0), (8.0, 7.0)];
            sh.extend(arc(12.0, 7.0, 4.0, 180.0, 360.0));
            sh.push((16.0, 5.0));
            line(&sh);
        }
        Icon::Bolt => {
            closed(&[
                (13.0, 2.5),
                (4.5, 13.5),
                (11.0, 13.5),
                (10.0, 21.5),
                (19.5, 9.5),
                (13.0, 9.5),
            ]);
        }
        Icon::WifiOff => {
            line(&arc(12.0, 19.0, 4.5, -135.0, -45.0));
            line(&arc(12.0, 19.0, 9.0, -135.0, -45.0));
            line(&arc(12.0, 19.0, 13.5, -135.0, -45.0));
            line(&[(12.0, 19.0), (12.01, 19.0)]);
            line(&[(3.0, 3.0), (21.0, 21.0)]);
        }
        Icon::Shield => {
            closed(&[
                (12.0, 3.0),
                (19.5, 6.0),
                (19.5, 12.0),
                (12.0, 21.0),
                (4.5, 12.0),
                (4.5, 6.0),
            ]);
            line(&[(9.5, 9.5), (14.5, 14.5)]);
            line(&[(14.5, 9.5), (9.5, 14.5)]);
        }
    }
}
