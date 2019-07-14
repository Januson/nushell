use crate::errors::ShellError;
use crate::parser::registry::CommandRegistry;
use crate::parser::{
    hir, hir::baseline_parse_single_token, DelimitedNode, Delimiter, PathNode, RawToken, Span,
    Spanned, TokenNode,
};
use crate::{SpannedItem, Text};
use derive_new::new;

pub fn baseline_parse_tokens(
    token_nodes: &mut TokensIterator<'_>,
    registry: &dyn CommandRegistry,
    source: &Text,
) -> Result<Vec<hir::Expression>, ShellError> {
    let mut exprs: Vec<hir::Expression> = vec![];

    loop {
        if token_nodes.at_end() {
            break;
        }

        let expr = baseline_parse_next_expr(token_nodes, registry, source, None)?;
        exprs.push(expr);
    }

    Ok(exprs)
}

#[allow(unused)]
#[derive(Debug)]
pub enum ExpressionKindHint {
    Literal,
    Variable,
    Binary,
    Block,
    Boolean,
}

pub fn baseline_parse_next_expr(
    tokens: &mut TokensIterator,
    registry: &dyn CommandRegistry,
    source: &Text,
    coerce_hint: Option<ExpressionKindHint>,
) -> Result<hir::Expression, ShellError> {
    let first = match tokens.next() {
        None => return Err(ShellError::string("Expected token, found none")),
        Some(token) => baseline_parse_semantic_token(token, registry, source)?,
    };

    let possible_op = tokens.peek();

    let op = match possible_op {
        Some(TokenNode::Operator(op)) => op.clone(),
        _ => return Ok(first),
    };

    tokens.next();

    let second = match tokens.next() {
        None => {
            return Err(ShellError::maybe_labeled_error(
                "Expected something after an operator",
                "operator",
                Some(op.span),
            ))
        }
        Some(token) => baseline_parse_semantic_token(token, registry, source)?,
    };

    // We definitely have a binary expression here -- let's see if we should coerce it into a block

    match coerce_hint {
        None => {
            let span = (first.span.start, second.span.end);
            let binary = hir::Binary::new(first, op, second);
            let binary = hir::RawExpression::Binary(Box::new(binary));
            let binary = Spanned::from_item(binary, span);

            Ok(binary)
        }

        Some(hint) => match hint {
            ExpressionKindHint::Block => {
                let span = (first.span.start, second.span.end);

                let path: Spanned<hir::RawExpression> = match first {
                    Spanned {
                        item: hir::RawExpression::Literal(hir::Literal::Bare),
                        span,
                    } => {
                        let string = Spanned::from_item(span.slice(source).to_string(), span);
                        let path = hir::Path::new(
                            Spanned::from_item(
                                // TODO: Deal with synthetic nodes that have no representation at all in source
                                hir::RawExpression::Variable(hir::Variable::It(Span::from((0, 0)))),
                                (0, 0),
                            ),
                            vec![string],
                        );
                        let path = hir::RawExpression::Path(Box::new(path));
                        Spanned {
                            item: path,
                            span: first.span,
                        }
                    }
                    Spanned {
                        item: hir::RawExpression::Literal(hir::Literal::String(inner)),
                        span,
                    } => {
                        let string = Spanned::from_item(inner.slice(source).to_string(), span);
                        let path = hir::Path::new(
                            Spanned::from_item(
                                // TODO: Deal with synthetic nodes that have no representation at all in source
                                hir::RawExpression::Variable(hir::Variable::It(Span::from((0, 0)))),
                                (0, 0),
                            ),
                            vec![string],
                        );
                        let path = hir::RawExpression::Path(Box::new(path));
                        Spanned {
                            item: path,
                            span: first.span,
                        }
                    }
                    Spanned {
                        item: hir::RawExpression::Variable(..),
                        ..
                    } => first,
                    Spanned { span, item } => {
                        return Err(ShellError::labeled_error(
                            "The first part of an un-braced block must be a column name",
                            item.type_name(),
                            span,
                        ))
                    }
                };

                let binary = hir::Binary::new(path, op, second);
                let binary = hir::RawExpression::Binary(Box::new(binary));
                let binary = Spanned::from_item(binary, span);

                let block = hir::RawExpression::Block(vec![binary]);
                let block = Spanned::from_item(block, span);

                Ok(block)
            }

            other => unimplemented!("coerce hint {:?}", other),
        },
    }
}

pub fn baseline_parse_semantic_token(
    token: &TokenNode,
    registry: &dyn CommandRegistry,
    source: &Text,
) -> Result<hir::Expression, ShellError> {
    match token {
        TokenNode::Token(token) => Ok(baseline_parse_single_token(token, source)),
        TokenNode::Call(_call) => unimplemented!(),
        TokenNode::Delimited(delimited) => baseline_parse_delimited(delimited, registry, source),
        TokenNode::Pipeline(_pipeline) => unimplemented!(),
        TokenNode::Operator(_op) => unreachable!(),
        TokenNode::Flag(_flag) => Err(ShellError::unimplemented("passing flags is not supported yet.")),
        TokenNode::Identifier(_span) => unreachable!(),
        TokenNode::Whitespace(_span) => unreachable!(),
        TokenNode::Error(error) => Err(*error.item.clone()),
        TokenNode::Path(path) => baseline_parse_path(path, registry, source),
    }
}

pub fn baseline_parse_delimited(
    token: &Spanned<DelimitedNode>,
    registry: &dyn CommandRegistry,
    source: &Text,
) -> Result<hir::Expression, ShellError> {
    match token.delimiter() {
        Delimiter::Brace => {
            let children = token.children();
            let exprs =
                baseline_parse_tokens(&mut TokensIterator::new(children), registry, source)?;

            let expr = hir::RawExpression::Block(exprs);
            Ok(Spanned::from_item(expr, token.span()))
        }
        Delimiter::Paren => unimplemented!(),
        Delimiter::Square => unimplemented!(),
    }
}

pub fn baseline_parse_path(
    token: &Spanned<PathNode>,
    registry: &dyn CommandRegistry,
    source: &Text,
) -> Result<hir::Expression, ShellError> {
    let head = baseline_parse_semantic_token(token.head(), registry, source)?;

    let mut tail = vec![];

    for part in token.tail() {
        let string = match part {
            TokenNode::Token(token) => match token.item() {
                RawToken::Bare => token.span().slice(source),
                RawToken::String(span) => span.slice(source),
                RawToken::Integer(_) | RawToken::Size(..) | RawToken::Variable(_) => {
                    return Err(ShellError::type_error(
                        "String",
                        token.type_name().spanned(part),
                    ))
                }
            },

            TokenNode::Identifier(span) => span.slice(source),

            // TODO: Make this impossible
            other => unreachable!("{:?}", other),
        }
        .to_string();

        tail.push(string.spanned(part));
    }

    Ok(hir::path(head, tail).spanned(token).into())
}

#[derive(Debug, new)]
pub struct TokensIterator<'a> {
    tokens: &'a [TokenNode],
    #[new(default)]
    index: usize,
    #[new(default)]
    seen: indexmap::IndexSet<usize>,
}

impl TokensIterator<'a> {
    pub fn remove(&mut self, position: usize) {
        self.seen.insert(position);
    }

    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    pub fn at_end(&self) -> bool {
        for index in self.index..self.tokens.len() {
            if !self.seen.contains(&index) {
                return false;
            }
        }

        true
    }

    pub fn advance(&mut self) {
        self.seen.insert(self.index);
        self.index += 1;
    }

    pub fn extract<T>(&mut self, f: impl Fn(&TokenNode) -> Option<T>) -> Option<(usize, T)> {
        for (i, item) in self.tokens.iter().enumerate() {
            if self.seen.contains(&i) {
                continue;
            }

            match f(item) {
                None => {
                    continue;
                }
                Some(value) => {
                    self.seen.insert(i);
                    return Some((i, value));
                }
            }
        }

        None
    }

    pub fn move_to(&mut self, pos: usize) {
        self.index = pos;
    }

    pub fn restart(&mut self) {
        self.index = 0;
    }

    pub fn clone(&self) -> TokensIterator {
        TokensIterator {
            tokens: self.tokens,
            index: self.index,
            seen: self.seen.clone(),
        }
    }

    pub fn peek(&self) -> Option<&TokenNode> {
        let mut tokens = self.clone();

        tokens.next()
    }

    pub fn debug_remaining(&self) -> Vec<TokenNode> {
        let mut tokens = self.clone();
        tokens.restart();
        tokens.cloned().collect()
    }
}

impl Iterator for TokensIterator<'a> {
    type Item = &'a TokenNode;

    fn next(&mut self) -> Option<&'a TokenNode> {
        loop {
            if self.index >= self.tokens.len() {
                return None;
            }

            if self.seen.contains(&self.index) {
                self.advance();
                continue;
            }

            if self.index >= self.tokens.len() {
                return None;
            }

            match &self.tokens[self.index] {
                TokenNode::Whitespace(_) => {
                    self.advance();
                }
                other => {
                    self.advance();
                    return Some(other);
                }
            }
        }
    }
}