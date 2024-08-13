use uuid::Uuid;
use wasmer::{self};
use wasmer_wasix::{WasiEnv, WasiFunctionEnv};

pub struct WasmProcess {
    function: wasmer::Function,
    env: WasiFunctionEnv,
}

impl WasmProcess {
    pub fn new(wasi_env: WasiFunctionEnv, function: wasmer::Function) -> Self {
        Self {
            function: function,
            env: wasi_env,
        }
    }

    pub fn run(
        &mut self,
        store: &mut wasmer::Store,
    ) -> Result<Box<[wasmer::Value]>, wasmer::RuntimeError> {
        let exit_code = self.function.call(store, &[])?;
        self.env.on_exit(store, None);
        Ok(exit_code)
    }
}

pub struct WasmRuntime {
    store: wasmer::Store,
}

impl WasmRuntime {
    pub fn new() -> Self {
        Self {
            store: wasmer::Store::default(),
        }
    }

    pub fn store(&mut self) -> &mut wasmer::Store {
        &mut self.store
    }

    pub fn build(&mut self, bytecode: Vec<u8>) -> Result<WasmProcess, Box<dyn std::error::Error>> {
        let module = wasmer::Module::new(&self.store, bytecode).expect("couldn't load WASM module");
        let uuid = Uuid::new_v4();
        let mut wasi_env = WasiEnv::builder(uuid)
            // .args(&["arg1", "arg2"])
            // .env("KEY", "VALUE")
            .finalize(&mut self.store)?;
        let import_object = wasi_env.import_object(&mut self.store, &module)?;
        let instance = wasmer::Instance::new(&mut self.store, &module, &import_object)?;

        // // Attach the memory export
        // let memory = instance.exports.get_memory("memory")?;
        // wasi_env.data_mut(&mut store).set_memory(memory.clone());

        wasi_env.initialize(&mut self.store, instance.clone())?;

        let function = instance
            .exports
            .get_function("_start")
            .map(|f| f.to_owned())?;

        Ok(WasmProcess::new(wasi_env, function))
    }
}
