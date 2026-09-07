Declare the `stream` feature on `reqwest` and add `futures-util` as a dependency. The sidecar download reads the response body in chunks so it can enforce its size cap before buffering, and `Response::bytes_stream` lives behind that feature, so the crate did not build without it.

Compare the `Content-Length` header against the cap by narrowing the header to `usize` rather than widening the cap with `as`. A length that does not fit in a `usize` cannot fit under the cap either, so it now counts as too large instead of wrapping.
