//! A simple [`Edge`] type describing the outlet and inlet of source and
//! destination nodes respectively.

use crate::CaHash;
use serde::{Deserialize, Serialize};

/// Represents an input of a node via an index.
#[derive(
    Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, Ord, Deserialize, Serialize, CaHash,
)]
pub struct Input(pub u16);

/// Represents an output of a node via an index.
#[derive(
    Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, Ord, Deserialize, Serialize, CaHash,
)]
pub struct Output(pub u16);

/// Describes a connection between two nodes.
#[derive(
    Copy, Clone, Debug, Eq, Hash, PartialEq, PartialOrd, Ord, Deserialize, Serialize, CaHash,
)]
pub struct Edge {
    /// The output of the node at the source of this edge.
    pub output: Output,
    /// The input of the node at the destination of this edge.
    pub input: Input,
}

impl Edge {
    /// Create an edge representing a connection from the given node `Output` to
    /// the given node `Input`.
    pub fn new(output: Output, input: Input) -> Self {
        Edge { output, input }
    }
}

impl From<u16> for Input {
    fn from(u: u16) -> Self {
        Input(u)
    }
}

impl From<u16> for Output {
    fn from(u: u16) -> Self {
        Output(u)
    }
}

impl<A, B> From<(A, B)> for Edge
where
    A: Into<Output>,
    B: Into<Input>,
{
    fn from((a, b): (A, B)) -> Self {
        let output = a.into();
        let input = b.into();
        Edge { output, input }
    }
}
