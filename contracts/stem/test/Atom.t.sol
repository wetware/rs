// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.24;

import { Test } from "forge-std/Test.sol";
import { Atom } from "../src/Atom.sol";

contract AtomTest is Test {
    Atom public atom;
    address public owner;
    address public user;

    function setUp() public {
        owner = address(this);
        user = address(0x123);
        atom = new Atom(bytes("ipfs-initial"));
    }

    function test_Constructor() public view {
        assertEq(atom.owner(), owner);
        assertEq(atom.seq(), 0);
        (uint64 s, bytes memory c) = atom.head();
        assertEq(s, 0);
        assertEq(keccak256(c), keccak256(bytes("ipfs-initial")));
    }

    function test_SetHead() public {
        bytes memory newCid = bytes("ipfs://new");
        bytes32 expectedCidHash = keccak256(newCid);
        vm.expectEmit(true, true, true, true);
        emit Atom.HeadUpdated(1, owner, newCid, expectedCidHash);

        atom.setHead(newCid);

        assertEq(atom.seq(), 1);
        (uint64 seq, bytes memory cid) = atom.head();
        assertEq(seq, 1);
        assertEq(cid, newCid);
    }

    function test_SetHead_NotOwner() public {
        vm.prank(user);
        vm.expectRevert(Atom.NotOwner.selector);
        atom.setHead(bytes("ipfs://new"));
    }

    function test_SetHead_NoChange() public {
        // Same cid as initial head -> revert NoChange, seq unchanged
        vm.expectRevert(Atom.NoChange.selector);
        atom.setHead(bytes("ipfs-initial"));
        assertEq(atom.seq(), 0);

        // One real update, then no-op again -> revert NoChange, seq still 1
        atom.setHead(bytes("ipfs://new"));
        assertEq(atom.seq(), 1);
        vm.expectRevert(Atom.NoChange.selector);
        atom.setHead(bytes("ipfs://new"));
        assertEq(atom.seq(), 1);
    }

    function test_SetHead_MultipleUpdates() public {
        atom.setHead(bytes("ipfs://first"));
        assertEq(atom.seq(), 1);

        atom.setHead(bytes("ipld://second"));
        assertEq(atom.seq(), 2);

        atom.setHead(bytes("blob://third"));
        assertEq(atom.seq(), 3);

        (uint64 seq, bytes memory cid) = atom.head();
        assertEq(seq, 3);
        assertEq(cid, bytes("blob://third"));
    }

    function test_SetHead_IPFS_UNIXFS() public {
        bytes memory cid = bytes("QmIPFS");
        bytes32 cidHash = keccak256(cid);
        vm.expectEmit(true, true, true, true);
        emit Atom.HeadUpdated(1, owner, cid, cidHash);

        atom.setHead(cid);

        assertEq(atom.seq(), 1);
        (uint64 seq, bytes memory c) = atom.head();
        assertEq(seq, 1);
        assertEq(c, cid);
    }

    function test_SetHead_IPLD_NODE() public {
        bytes memory cid = bytes("QmIPLD");
        bytes32 cidHash = keccak256(cid);
        vm.expectEmit(true, true, true, true);
        emit Atom.HeadUpdated(1, owner, cid, cidHash);

        atom.setHead(cid);

        assertEq(atom.seq(), 1);
        (uint64 seq, bytes memory c) = atom.head();
        assertEq(seq, 1);
        assertEq(c, cid);
    }

    function test_SetHead_BLOB() public {
        bytes memory cid = bytes("blob-data");
        bytes32 cidHash = keccak256(cid);
        vm.expectEmit(true, true, true, true);
        emit Atom.HeadUpdated(1, owner, cid, cidHash);

        atom.setHead(cid);

        assertEq(atom.seq(), 1);
        (uint64 seq, bytes memory c) = atom.head();
        assertEq(seq, 1);
        assertEq(c, cid);
    }

    function test_SetHead_IPNS_NAME() public {
        bytes memory cid = bytes("k51qzi5uqu5...");
        bytes32 cidHash = keccak256(cid);
        vm.expectEmit(true, true, true, true);
        emit Atom.HeadUpdated(1, owner, cid, cidHash);

        atom.setHead(cid);

        assertEq(atom.seq(), 1);
        (uint64 seq, bytes memory c) = atom.head();
        assertEq(seq, 1);
        assertEq(c, cid);
    }
}
