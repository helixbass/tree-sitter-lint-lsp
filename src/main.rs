use dashmap::DashMap;
use ropey::Rope;
use tower_lsp::{
    jsonrpc::Result,
    lsp_types::{
        Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidOpenTextDocumentParams,
        InitializeParams, InitializeResult, InitializedParams, NumberOrString, Position, Range,
        ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
    },
    Client, LanguageServer, LspService, Server,
};
use tree_sitter_lint::{
    tree_sitter::{self, InputEdit, Parser, Point, Tree},
    tree_sitter_grep::{Parseable, SupportedLanguage},
};

#[derive(Debug)]
struct Backend {
    client: Client,
    per_file: DashMap<Url, PerFileState>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            per_file: Default::default(),
        }
    }

    async fn run_linting_and_report_diagnostics(&self, uri: &Url) {
        let per_file_state = self.per_file.get(uri).unwrap();
        let violations = tree_sitter_lint_local::run_for_slice(
            &per_file_state.contents,
            Some(&per_file_state.tree),
            "dummy_path",
            Default::default(),
        );
        self.client
            .publish_diagnostics(
                uri.clone(),
                violations
                    .into_iter()
                    .map(|violation| Diagnostic {
                        message: violation.message,
                        range: tree_sitter_range_to_lsp_range(
                            &per_file_state.contents,
                            violation.range,
                        ),
                        severity: Some(DiagnosticSeverity::ERROR),
                        code: Some(NumberOrString::String(violation.rule.name)),
                        source: Some("tree-sitter-lint".to_owned()),
                        ..Default::default()
                    })
                    .collect(),
                None,
            )
            .await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        // self.client
        //     .log_message(tower_lsp::lsp_types::MessageType::INFO, "server initialized!")
        //     .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let contents: Rope = (&*params.text_document.text).into();
        self.per_file.insert(
            params.text_document.uri.clone(),
            PerFileState {
                tree: parse_from_scratch(&contents),
                contents,
            },
        );

        self.run_linting_and_report_diagnostics(&params.text_document.uri)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        {
            let mut file_state = self
                .per_file
                .get_mut(&params.text_document.uri)
                .expect("Changed file wasn't loaded");
            for content_change in &params.content_changes {
                match content_change.range {
                    Some(range) => {
                        let start_char =
                            lsp_position_to_char_offset(&file_state.contents, range.start);
                        let end_char = lsp_position_to_char_offset(&file_state.contents, range.end);
                        let start_byte = file_state.contents.char_to_byte(start_char);
                        let old_end_byte = file_state.contents.char_to_byte(end_char);
                        file_state.contents.remove(start_char..end_char);
                        file_state.contents.insert(start_char, &content_change.text);

                        let new_end_byte = start_byte + content_change.text.len();
                        let input_edit = InputEdit {
                            start_byte,
                            old_end_byte,
                            new_end_byte,
                            start_position: position_to_point(range.start),
                            old_end_position: position_to_point(range.end),
                            new_end_position: byte_offset_to_point(
                                &file_state.contents,
                                new_end_byte,
                            ),
                        };
                        file_state.tree.edit(&input_edit);
                        file_state.tree = parse(&file_state.contents, Some(&file_state.tree));
                    }
                    None => {
                        file_state.contents = (&*content_change.text).into();
                        file_state.tree = parse_from_scratch(&file_state.contents);
                    }
                }
            }
        }

        self.run_linting_and_report_diagnostics(&params.text_document.uri)
            .await;
    }
}

#[derive(Debug)]
struct PerFileState {
    contents: Rope,
    tree: Tree,
}

fn parse_from_scratch(contents: &Rope) -> Tree {
    parse(contents, None)
}

fn parse(contents: &Rope, old_tree: Option<&Tree>) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(SupportedLanguage::Rust.language())
        .unwrap();
    contents.parse(&mut parser, old_tree).unwrap()
}

fn lsp_position_to_char_offset(file_contents: &Rope, position: Position) -> usize {
    file_contents.line_to_char(position.line as usize) + position.character as usize
}

fn position_to_point(position: Position) -> Point {
    Point {
        row: position.line as usize,
        column: position.character as usize,
    }
}

fn point_to_position(point: Point) -> Position {
    Position {
        line: point.row as u32,
        character: point.column as u32,
    }
}

fn byte_offset_to_point(file_contents: &Rope, byte_offset: usize) -> Point {
    let line_num = file_contents.byte_to_line(byte_offset);
    let start_of_line_byte_offset = file_contents.line_to_byte(line_num);
    Point {
        row: line_num,
        column: byte_offset - start_of_line_byte_offset,
    }
}

fn tree_sitter_range_to_lsp_range(file_contents: &Rope, range: tree_sitter::Range) -> Range {
    Range {
        start: point_to_position(byte_offset_to_point(file_contents, range.start_byte)),
        end: point_to_position(byte_offset_to_point(file_contents, range.end_byte)),
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
