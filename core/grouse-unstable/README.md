# grouse-unstable

The goose-fork compatibility shim: every `_goose/unstable/*` method, plus the
SDK's `unstable_elicitation` feature and the goose-custom `status_message` /
`message_usage` notifications.

This crate is temporary. It exists so existing goose-fork servers keep working
while the official goose GDK absorbs each feature upstream. Retire each method
(and move its consumers to the GDK surface) as it lands upstream.
