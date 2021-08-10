//! Source files.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[cfg(feature = "codespan-reporting")]
use codespan_reporting::files::{self, Files};
use serde::{Deserialize, Serialize};

use crate::loading::{FileHash, Loader};
use crate::parse::{is_newline, Scanner};
use crate::syntax::{Pos, Span};
use crate::util::PathExt;

/// A unique identifier for a loaded source file.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[derive(Serialize, Deserialize)]
pub struct SourceId(u32);

impl SourceId {
    /// Create a source id from the raw underlying value.
    ///
    /// This should only be called with values returned by
    /// [`into_raw`](Self::into_raw).
    pub const fn from_raw(v: u32) -> Self {
        Self(v)
    }

    /// Convert into the raw underlying value.
    pub const fn into_raw(self) -> u32 {
        self.0
    }
}

/// Storage for loaded source files.
pub struct SourceStore {
    loader: Rc<dyn Loader>,
    files: HashMap<FileHash, SourceId>,
    sources: Vec<SourceFile>,
}

impl SourceStore {
    /// Create a new, empty source store.
    pub fn new(loader: Rc<dyn Loader>) -> Self {
        Self {
            loader,
            files: HashMap::new(),
            sources: vec![],
        }
    }

    /// Load a source file from a path using the `loader`.
    pub fn load(&mut self, path: &Path) -> io::Result<SourceId> {
        let hash = self.loader.resolve(path)?;
        if let Some(&id) = self.files.get(&hash) {
            return Ok(id);
        }

        let data = self.loader.load(path)?;
        let src = String::from_utf8(data).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "file is not valid utf-8")
        })?;

        Ok(self.insert(path, src, Some(hash)))
    }

    /// Directly provide a source file.
    ///
    /// The `path` does not need to be [resolvable](Loader::resolve) through the
    /// `loader`. If it is though, imports that resolve to the same file hash
    /// will use the inserted file instead of going through [`Loader::load`].
    ///
    /// If the path is resolvable and points to an existing source file, it is
    /// overwritten.
    pub fn provide(&mut self, path: &Path, src: String) -> SourceId {
        if let Ok(hash) = self.loader.resolve(path) {
            if let Some(&id) = self.files.get(&hash) {
                // Already loaded, so we replace it.
                self.sources[id.0 as usize] = SourceFile::new(id, path, src);
                id
            } else {
                // Not loaded yet.
                self.insert(path, src, Some(hash))
            }
        } else {
            // Not known to the loader.
            self.insert(path, src, None)
        }
    }

    /// Insert a new source file.
    fn insert(&mut self, path: &Path, src: String, hash: Option<FileHash>) -> SourceId {
        let id = SourceId(self.sources.len() as u32);
        if let Some(hash) = hash {
            self.files.insert(hash, id);
        }
        self.sources.push(SourceFile::new(id, path, src));
        id
    }

    /// Get a reference to a loaded source file.
    ///
    /// This panics if no source file with this id was loaded. This function
    /// should only be called with ids returned by this store's
    /// [`load()`](Self::load) and [`provide()`](Self::provide) methods.
    #[track_caller]
    pub fn get(&self, id: SourceId) -> &SourceFile {
        &self.sources[id.0 as usize]
    }
}

/// A single source file.
///
/// _Note_: All line and column indices start at zero, just like byte indices.
/// Only for user-facing display, you should add 1 to them.
pub struct SourceFile {
    id: SourceId,
    path: PathBuf,
    src: String,
    line_starts: Vec<Pos>,
}

impl SourceFile {
    /// Create a new source file.
    pub fn new(id: SourceId, path: &Path, src: String) -> Self {
        let mut line_starts = vec![Pos::ZERO];
        let mut s = Scanner::new(&src);

        while let Some(c) = s.eat() {
            if is_newline(c) {
                if c == '\r' {
                    s.eat_if('\n');
                }
                line_starts.push(s.index().into());
            }
        }

        Self {
            id,
            path: path.normalize(),
            src,
            line_starts,
        }
    }

    /// Create a source file without a real id and path, usually for testing.
    pub fn detached(src: impl Into<String>) -> Self {
        Self::new(SourceId(0), Path::new(""), src.into())
    }

    /// The id of the source file.
    pub fn id(&self) -> SourceId {
        self.id
    }

    /// The normalized path to the source file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The whole source as a string slice.
    pub fn src(&self) -> &str {
        &self.src
    }

    /// Slice out the part of the source code enclosed by the span.
    pub fn get(&self, span: impl Into<Span>) -> Option<&str> {
        self.src.get(span.into().to_range())
    }

    /// Get the length of the file in bytes.
    pub fn len_bytes(&self) -> usize {
        self.src.len()
    }

    /// Get the length of the file in lines.
    pub fn len_lines(&self) -> usize {
        self.line_starts.len()
    }

    /// Return the index of the line that contains the given byte position.
    pub fn pos_to_line(&self, byte_pos: Pos) -> Option<usize> {
        (byte_pos.to_usize() <= self.src.len()).then(|| {
            match self.line_starts.binary_search(&byte_pos) {
                Ok(i) => i,
                Err(i) => i - 1,
            }
        })
    }

    /// Return the index of the column at the byte index.
    ///
    /// The column is defined as the number of characters in the line before the
    /// byte position.
    pub fn pos_to_column(&self, byte_pos: Pos) -> Option<usize> {
        let line = self.pos_to_line(byte_pos)?;
        let start = self.line_to_pos(line)?;
        let head = self.get(Span::new(start, byte_pos))?;
        Some(head.chars().count())
    }

    /// Return the byte position at which the given line starts.
    pub fn line_to_pos(&self, line_idx: usize) -> Option<Pos> {
        self.line_starts.get(line_idx).copied()
    }

    /// Return the span which encloses the given line.
    pub fn line_to_span(&self, line_idx: usize) -> Option<Span> {
        let start = self.line_to_pos(line_idx)?;
        let end = self.line_to_pos(line_idx + 1).unwrap_or(self.src.len().into());
        Some(Span::new(start, end))
    }

    /// Return the byte position of the given (line, column) pair.
    ///
    /// The column defines the number of characters to go beyond the start of
    /// the line.
    pub fn line_column_to_pos(&self, line_idx: usize, column_idx: usize) -> Option<Pos> {
        let span = self.line_to_span(line_idx)?;
        let line = self.get(span)?;
        let mut chars = line.chars();
        for _ in 0 .. column_idx {
            chars.next();
        }
        Some(span.start + (line.len() - chars.as_str().len()))
    }
}

impl AsRef<str> for SourceFile {
    fn as_ref(&self) -> &str {
        &self.src
    }
}

#[cfg(feature = "codespan-reporting")]
impl<'a> Files<'a> for SourceStore {
    type FileId = SourceId;
    type Name = std::path::Display<'a>;
    type Source = &'a SourceFile;

    fn name(&'a self, id: SourceId) -> Result<Self::Name, files::Error> {
        Ok(self.get(id).path().display())
    }

    fn source(&'a self, id: SourceId) -> Result<Self::Source, files::Error> {
        Ok(self.get(id))
    }

    fn line_index(&'a self, id: SourceId, given: usize) -> Result<usize, files::Error> {
        let source = self.get(id);
        source
            .pos_to_line(given.into())
            .ok_or_else(|| files::Error::IndexTooLarge { given, max: source.len_bytes() })
    }

    fn line_range(
        &'a self,
        id: SourceId,
        given: usize,
    ) -> Result<std::ops::Range<usize>, files::Error> {
        let source = self.get(id);
        source
            .line_to_span(given)
            .map(Span::to_range)
            .ok_or_else(|| files::Error::LineTooLarge { given, max: source.len_lines() })
    }

    fn column_number(
        &'a self,
        id: SourceId,
        _: usize,
        given: usize,
    ) -> Result<usize, files::Error> {
        let source = self.get(id);
        source.pos_to_column(given.into()).ok_or_else(|| {
            let max = source.len_bytes();
            if given <= max {
                files::Error::InvalidCharBoundary { given }
            } else {
                files::Error::IndexTooLarge { given, max }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST: &str = "ä\tcde\nf💛g\r\nhi\rjkl";

    #[test]
    fn test_source_file_new() {
        let source = SourceFile::detached(TEST);
        assert_eq!(source.line_starts, vec![Pos(0), Pos(7), Pos(15), Pos(18)]);
    }

    #[test]
    fn test_source_file_pos_to_line() {
        let source = SourceFile::detached(TEST);
        assert_eq!(source.pos_to_line(Pos(0)), Some(0));
        assert_eq!(source.pos_to_line(Pos(2)), Some(0));
        assert_eq!(source.pos_to_line(Pos(6)), Some(0));
        assert_eq!(source.pos_to_line(Pos(7)), Some(1));
        assert_eq!(source.pos_to_line(Pos(8)), Some(1));
        assert_eq!(source.pos_to_line(Pos(12)), Some(1));
        assert_eq!(source.pos_to_line(Pos(21)), Some(3));
        assert_eq!(source.pos_to_line(Pos(22)), None);
    }

    #[test]
    fn test_source_file_pos_to_column() {
        let source = SourceFile::detached(TEST);
        assert_eq!(source.pos_to_column(Pos(0)), Some(0));
        assert_eq!(source.pos_to_column(Pos(2)), Some(1));
        assert_eq!(source.pos_to_column(Pos(6)), Some(5));
        assert_eq!(source.pos_to_column(Pos(7)), Some(0));
        assert_eq!(source.pos_to_column(Pos(8)), Some(1));
        assert_eq!(source.pos_to_column(Pos(12)), Some(2));
    }

    #[test]
    fn test_source_file_roundtrip() {
        #[track_caller]
        fn roundtrip(source: &SourceFile, byte_pos: Pos) {
            let line = source.pos_to_line(byte_pos).unwrap();
            let column = source.pos_to_column(byte_pos).unwrap();
            let result = source.line_column_to_pos(line, column).unwrap();
            assert_eq!(result, byte_pos);
        }

        let source = SourceFile::detached(TEST);
        roundtrip(&source, Pos(0));
        roundtrip(&source, Pos(7));
        roundtrip(&source, Pos(12));
        roundtrip(&source, Pos(21));
    }
}
