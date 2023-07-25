use dashmap::DashMap;
use ropey::Rope;
use tower_lsp::{
    jsonrpc::Result,
    lsp_types::{
        DidChangeTextDocumentParams, DidOpenTextDocumentParams, InitializeParams, InitializeResult,
        InitializedParams, MessageType, Position, Url,
    },
    Client, LanguageServer, LspService, Server,
};
use tree_sitter_lint::{
    tree_sitter::{InputEdit, Parser, Point, Tree},
    tree_sitter_grep::SupportedLanguage,
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
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(Default::default())
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "server initialized!")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let contents: Rope = (&*params.text_document.text).into();
        self.per_file.insert(
            params.text_document.uri,
            PerFileState {
                tree: parse_from_scratch(&contents),
                contents,
            },
        );
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let mut file_state = self
            .per_file
            .get_mut(&params.text_document.uri)
            .expect("Changed file wasn't loaded");
        assert!(
            params.content_changes.len() == 1,
            "Only handling single content change currently"
        );
        let content_change = &params.content_changes[0];
        match content_change.range {
            Some(range) => {
                let start_char = lsp_position_to_char_offset(&file_state.contents, range.start);
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
                    new_end_position: byte_offset_to_point(&file_state.contents, new_end_byte),
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
    parser
        .parse_with(
            &mut |byte_offset, _| {
                let (chunk, chunk_start_byte_index, _, _) = contents.chunk_at_byte(byte_offset);
                &chunk[byte_offset - chunk_start_byte_index..]
            },
            old_tree,
        )
        .unwrap()
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

fn byte_offset_to_point(file_contents: &Rope, byte_offset: usize) -> Point {
    let line_num = file_contents.byte_to_line(byte_offset);
    let start_of_line_byte_offset = file_contents.line_to_byte(line_num);
    Point {
        row: line_num,
        column: byte_offset - start_of_line_byte_offset,
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
