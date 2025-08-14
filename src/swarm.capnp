@0x990392714b7ccd36;

using ServiceToken = Data;

interface Exporter {
    export @0 (service :Capability) -> (token :ServiceToken);
}

interface Importer {
    import @0 (token :ServiceToken) -> (service :Capability);
}
