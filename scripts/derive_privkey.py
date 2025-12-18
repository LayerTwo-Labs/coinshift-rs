#!/usr/bin/env python3
"""
Derive a WIF private key from an extended private key (xprv/tprv) and BIP32 path.
Requires: pip install base58
"""
import sys
import hmac
import hashlib

try:
    import base58
except ImportError:
    print("ERROR: base58 library required. Install with: pip install base58", file=sys.stderr)
    sys.exit(1)

def derive_private_key(xprv_str, keypath):
    """Derive private key from extended private key and BIP32 path"""
    try:
        # Decode the extended private key
        decoded = base58.b58decode(xprv_str)
        if len(decoded) != 78:
            return None
        
        # Extract chain code and key (BIP32 format)
        key = decoded[46:78]
        chain_code = decoded[13:45]
        
        # Parse keypath (e.g., "m/84h/1h/0h/0/0")
        path_parts = keypath.replace("m/", "").split("/")
        
        current_key = key
        current_chain = chain_code
        
        for part in path_parts:
            if not part:
                continue
            # Handle hardened (h or ')
            if part.endswith("h") or part.endswith("'"):
                index = int(part[:-1]) + 0x80000000
            else:
                index = int(part)
            
            # BIP32 CKDpriv: HMAC-SHA512
            data = b'\x00' + current_key + index.to_bytes(4, 'big')
            h = hmac.new(current_chain, data, hashlib.sha512).digest()
            current_key = h[:32]
            current_chain = h[32:]
        
        # Convert to WIF (Wallet Import Format)
        # For testnet/regtest, version byte is 0xef
        version_byte = b'\xef'
        extended = version_byte + current_key + b'\x01'  # compressed flag
        checksum = hashlib.sha256(hashlib.sha256(extended).digest()).digest()[:4]
        wif = base58.b58encode(extended + checksum).decode('ascii')
        return wif
    except Exception as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return None

if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("Usage: derive_privkey.py <xprv_or_tprv> <keypath>", file=sys.stderr)
        print("Example: derive_privkey.py tprv8Zgx... m/84h/1h/0h/0/0", file=sys.stderr)
        sys.exit(1)
    
    xprv = sys.argv[1]
    keypath = sys.argv[2]
    result = derive_private_key(xprv, keypath)
    if result:
        print(result)
    else:
        sys.exit(1)
