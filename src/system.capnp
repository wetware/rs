@0xda965b22da734daf;

interface Importer {
    import @0 (serviceToken :Data) -> (service :Capability);
}

interface Exporter {
    export @0 (service :Capability) -> (serviceToken :Data);
}
