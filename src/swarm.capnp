@0x990392714b7ccd36;


interface Exporter {
    export @0 (service :Capability) -> (token :Data);
}

interface Importer {
    import @0 (token :Data) -> (service :Capability);
}
