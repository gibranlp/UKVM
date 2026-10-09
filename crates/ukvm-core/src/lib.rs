//! Platform and transport independent node policy.

pub mod clipboard;
use ukvm_protocol::PeerId;

/// Permissions of this installation. A node can be both at once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Controller,
    Controlled,
    Both,
}

impl Role {
    pub const fn can_control(self) -> bool {
        matches!(self, Self::Controller | Self::Both)
    }

    pub const fn can_be_controlled(self) -> bool {
        matches!(self, Self::Controlled | Self::Both)
    }
}

/// Current focus owner. Transitions must be authorized by the future session layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Focus {
    Local,
    Remote(PeerId),
}

/// A direction in the logical layout, independent of display coordinates.
pub use ukvm_protocol::Edge;

#[cfg(test)]
mod tests {
    use super::Role;

    #[test]
    fn roles_express_both_capabilities() {
        assert!(Role::Both.can_control());
        assert!(Role::Both.can_be_controlled());
        assert!(!Role::Controller.can_be_controlled());
        assert!(!Role::Controlled.can_control());
    }
}
pub mod controlled;

/// Logical topology coordinates; screen pixels belong to platform backends.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct Coordinate {
    pub x: i32,
    pub y: i32,
}
impl Coordinate {
    pub const LOCAL: Self = Self { x: 0, y: 0 };
    pub fn adjacent(self, edge: ukvm_protocol::Edge) -> Self {
        let (dx, dy) = match edge {
            ukvm_protocol::Edge::Left => (-1, 0),
            ukvm_protocol::Edge::Right => (1, 0),
            ukvm_protocol::Edge::Top => (0, -1),
            ukvm_protocol::Edge::Bottom => (0, 1),
        };
        Self {
            x: self.x + dx,
            y: self.y + dy,
        }
    }
}
impl std::fmt::Display for Coordinate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({},{})", self.x, self.y)
    }
}
