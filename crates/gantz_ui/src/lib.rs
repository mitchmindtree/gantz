//! The declarative UI tree model for user-defined gantz GUIs.
//!
//! A graph's GUI is a value: a tree of plain data that the graph itself
//! produces, interpreted by a host every frame. This crate defines the
//! canonical typed model of that tree along with per-runtime codecs. It is
//! deliberately free of any GUI toolkit and, at its core, free of any
//! runtime.
//!
//! The full v1 vocabulary reference lives here at the crate level and grows
//! alongside the modules below.

pub use diag::{ErrorReason, TreePath, Warning, WarningKind};
pub use elem::{
    ATTRS_MARKER, Align, BindPath, Button, Col, Dialer, DialerStyle, Element, ErrorElem, Frame,
    Grid, Key, Label, Matrix, Plot, PlotMode, PlotStyle, RESERVED_TAGS, RefGui, Rgba, Row, Scope,
    Sep, Space, Toggle, Value,
};
pub use sexpr::{SExpr, summary};

pub mod diag;
pub mod elem;
pub mod sexpr;
