# {{PROJECT_NAME}} - Token Template

A fungible token program following the **community-standard `token.aleo`
pattern**, scaffolded by AleoFlow.

## Status: Community Standard, Not an Official ARC

This template implements the widely-followed `token.aleo` convention that is
the de facto starting point for fungible tokens on Aleo. It is **not** based
on an officially ratified ARC (Aleo Request for Comments) number. The pattern
has been adopted by many projects in the ecosystem, but it has not gone
through formal standardization. Always audit and verify token logic for your
specific use case, and check the latest ARC proposals before shipping.

## Why This Differs From ERC-20

If you are coming from Ethereum, the biggest conceptual shift is Aleo's
**UTXO/record model** for private balances:

| Concept | ERC-20 (Ethereum) | token.aleo (Aleo) |
|---|---|---|
| Balance storage | `mapping(address => uint256)` | Public: `mapping account: address => u64` |
| Private balance | Not supported | `record Token` (UTXO) |
| Balance query | `balanceOf(address)` | Public lookup OR record scan |
| Transfer model | Account-based deduction | UTXO record consumption |

**Critical warning:** Do not assume EVM-style balance-mapping-only logic
works here. Aleo supports two parallel balance systems:

- **Public balances** live in the `account` mapping and can be read
  on-chain, similar to ERC-20.
- **Private balances** live in `Token` records that users hold in their
  wallets. These records are **not** stored in a single on-chain mapping.
  To find a user's private Token records, you must scan the blockchain
  with `aleoflow records list` (which wraps `snarkos developer scan`)
  rather than doing a simple mapping lookup.

If your dApp only tracks public `account` mapping balances, it will miss all
privately-held tokens. Consider both systems.

## Transitions

| Function | Input | Output | Description |
|---|---|---|---|
| `mint` | `receiver`, `amount` | `Token` | Creates a new private Token record (ungated) |
| `transfer` | `sender: Token`, `receiver`, `amount` | `(Token, Token)` | Private UTXO transfer (change + transferred records) |
| `transfer_public` | `sender`, `receiver`, `amount` | `Final` | Public transfer via on-chain mapping |
| `private_to_public` | `sender: Token`, `receiver`, `amount` | `(Token, Final)` | Convert private tokens to public |
| `public_to_private` | `receiver`, `amount` | `(Token, Final)` | Convert public tokens to private |

> **Note on mint access control:** The `mint` function is intentionally
> left unrestricted in this template. In production, add access control
> (e.g. checking `self.caller` against an admin address or a roles mapping)
> to prevent unauthorized minting.

## Quick Start

```bash
cd {{PROJECT_NAME}}
leo build
leo test
```
