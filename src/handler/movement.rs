use crate::layout::Point;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MovementBehavior {
    LinearPath,
}

pub(crate) fn movement_path(
    start: Point,
    target: Point,
    behavior: &MovementBehavior,
) -> Vec<Point> {
    match behavior {
        MovementBehavior::LinearPath => linear_path(start, target),
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
