use std::any::Any;

use capnp::{Error, capability::Promise};
use capnp_rpc::{pry, rpc_twoparty_capnp, twoparty, RpcSystem};
use crate::proc_capnp::proc_;

use wasmer::{Instance, Store};




struct WasmerProc {
    store: Store,
    wasm: Instance
}

impl proc_::Server for WasmerProc {
    fn deliver(
        &mut self,
        params: proc_::DeliverParams,
        mut results: proc_::DeliverResults,
    ) ->  Promise<(), Error>{
        let method = pry!(pry!(pry!(params.get()).get_method()).to_str());
        let event = pry!(pry!(params.get()).get_event());

        match self.wasm.exports.get_function(method) {
            Ok(f) => {
                // f.call(&mut self.store, event)?;
                Promise::ok(())
            }
            Err(e) => {
                Promise::err(Error::failed(format!("method not found: {:?}", e)))
            }
        }
    }
}