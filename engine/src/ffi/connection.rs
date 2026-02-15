use super::{ffi_close, ffi_open};
use crate::dict::connection::ConnectionMatrix;

ffi_open!(lex_conn_open, ConnectionMatrix, |p| ConnectionMatrix::open(
    p
));
ffi_close!(lex_conn_close, ConnectionMatrix);
