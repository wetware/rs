// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Script.sol";
import "../src/Atom.sol";

contract Deploy is Script {
    function run() external {
        vm.startBroadcast();
        Atom atom = new Atom(bytes("ipfs-initial"));
        console.log("Atom deployed at:", address(atom));
        vm.stopBroadcast();
    }
}
