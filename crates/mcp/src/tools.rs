//! The 8 MCP tools, as a thin layer over `Backend`. Implemented in T3.4.

use std::marker::PhantomData;

use crate::backend::Backend;

pub struct CodoseoMcp<B: Backend> {
    _backend: PhantomData<B>,
}
