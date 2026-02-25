#[no_mangle]
pub extern "C" fn _start() {
    system::run::<capnp::capability::Client, _, _>(|_host| async move {
        // TODO: implement shell
        Ok(())
    });
}
