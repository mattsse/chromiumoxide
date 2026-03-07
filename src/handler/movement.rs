use crate::layout::Point;
#[cfg(feature = "human_movements")]
use rand::Rng;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MovementBehavior {
    LinearPath,
    BezierPath,
}

pub(crate) fn movement_path(
    start: Point,
    target: Point,
    behavior: &MovementBehavior,
) -> Vec<Point> {
    match behavior {
        MovementBehavior::LinearPath => linear_path(start, target),
        MovementBehavior::BezierPath => bezier_path(start, target),
    }
}

fn linear_path(start: Point, target: Point) -> Vec<Point> {
    let dx = target.x - start.x;
    let dy = target.y - start.y;
    let distance = (dx * dx + dy * dy).sqrt();
    let steps = ((distance / 32.0).ceil() as usize).clamp(2, 24);

    let mut points = Vec::with_capacity(steps);
    for step in 1..=steps {
        let t = step as f64 / steps as f64;
        points.push(Point::new(start.x + dx * t, start.y + dy * t));
    }
    points
}

#[cfg(not(feature = "human_movements"))]
fn bezier_path(start: Point, target: Point) -> Vec<Point> {
    tracing::warn!("human_movements feature does not enabled");
    vec![start, target]
}

#[cfg(feature = "human_movements")]
fn bezier_path(start: Point, target: Point) -> Vec<Point> {
    let mut rng = rand::rng();
    let mut path = Vec::with_capacity(steps);

    // Calculate distance for offset scaling
    let dist = ((end.x - start.x).powi(2) + (end.y - start.y).powi(2)).sqrt();
    let offset_range = dist * 0.3;

    // First control point (25% along the path with random offset)
    let p1 = Point {
        x: start.x + (end.x - start.x) * 0.25 + rng.random_range(-offset_range..offset_range),
        y: start.y + (end.y - start.y) * 0.25 + rng.random_range(-offset_range..offset_range),
    };

    // Second control point (75% along the path with random offset)
    // 20% chance of overshoot
    let mut p2 = Point {
        x: start.x + (end.x - start.x) * 0.75 + rng.random_range(-offset_range..offset_range),
        y: start.y + (end.y - start.y) * 0.75 + rng.random_range(-offset_range..offset_range),
    };

    if rng.random_bool(0.20) {
        let overshoot_amt = dist * 0.05;
        p2.x += if end.x > start.x {
            overshoot_amt
        } else {
            -overshoot_amt
        };
        p2.y += if end.y > start.y {
            overshoot_amt
        } else {
            -overshoot_amt
        };
    }

    // Generate points along the Bezier curve
    for i in 0..=steps {
        let t = i as f64 / steps as f64;

        // Cubic Bezier formula
        let x = (1.0 - t).powi(3) * start.x
            + 3.0 * (1.0 - t).powi(2) * t * p1.x
            + 3.0 * (1.0 - t) * t.powi(2) * p2.x
            + t.powi(3) * end.x;

        let y = (1.0 - t).powi(3) * start.y
            + 3.0 * (1.0 - t).powi(2) * t * p1.y
            + 3.0 * (1.0 - t) * t.powi(2) * p2.y
            + t.powi(3) * end.y;

        path.push(Point { x, y });
    }

    path
}
