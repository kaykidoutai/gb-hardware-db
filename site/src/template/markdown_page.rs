// SPDX-FileCopyrightText: 2017-2023 Joonas Javanainen <joonas.javanainen@gmail.com>
//
// SPDX-License-Identifier: MIT

use percy_dom::{html, IterableNodes, View, VirtualNode};

use super::markdown::Markdown;

pub struct MarkdownPage {
    pub markdown: Markdown,
}

impl View for MarkdownPage {
    fn render(&self) -> VirtualNode {
        html! {
            <article>
            {self.markdown.render()}
            </article>
        }
    }
}
