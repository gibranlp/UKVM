//! Platform and transport independent node policy.

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

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
