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

pub use sexpr::{SExpr, summary};

pub mod sexpr;
