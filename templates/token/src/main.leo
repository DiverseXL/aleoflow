// The {{PROJECT_NAME}} program
// A fungible token following the community-standard token.aleo pattern.
//
// NOTE: This template follows the de facto `token.aleo` convention widely
// used across the Aleo ecosystem. It is NOT an officially ratified ARC
// (Aleo Request for Comments) standard -- see README.md for details.

program {{PROJECT_NAME}}.aleo {
    @noupgrade
    constructor() {}

    // Public on-chain balances, tracked in the `account` mapping.
    // These are transparent: anyone can read them on-chain.
    mapping account: address => u64;

    // Private token record (Aleo's UTXO model). Holdings live inside this
    // record in a user's wallet and must be scanned for -- they are NOT
    // stored in a single on-chain mapping. See `aleoflow records list`.
    record Token {
        owner: address,
        amount: u64,
    }

    // Mint tokens privately, producing a new Token record for the receiver.
    // NOTE: Intentionally ungated for this template. Production use should
    // add access control (e.g. checking self.caller against an admin
    // address or a roles mapping) to prevent unauthorized minting.
    fn mint(receiver: address, amount: u64) -> Token {
        return Token {
            owner: receiver,
            amount: amount,
        };
    }

    // Private UTXO transfer: consumes the caller's Token record and returns
    // a change record (back to the sender) plus a transferred record (to
    // the receiver). No on-chain state is written.
    fn transfer(sender: Token, receiver: address, amount: u64) -> (Token, Token) {
        let difference: u64 = sender.amount - amount;
        let remaining: Token = Token {
            owner: sender.owner,
            amount: difference,
        };
        let transferred: Token = Token {
            owner: receiver,
            amount: amount,
        };
        return (remaining, transferred);
    }

    // Public transfer: decrements the sender's public `account` balance and
    // increments the receiver's, via the on-chain finalize block.
    fn transfer_public(public sender: address, public receiver: address, public amount: u64) -> Final {
        assert_eq(self.caller, sender);
        return final {
            let sender_amount: u64 = Mapping::get_or_use(account, sender, 0u64);
            Mapping::set(account, sender, sender_amount - amount);
            let receiver_amount: u64 = Mapping::get_or_use(account, receiver, 0u64);
            Mapping::set(account, receiver, receiver_amount + amount);
        };
    }

    // Convert private tokens to public: consumes a Token record and credits
    // the receiver's public `account` balance on-chain.
    // Pass self.caller as the receiver to convert to your own public balance.
    fn private_to_public(sender: Token, public receiver: address, public amount: u64) -> (Token, Final) {
        let difference: u64 = sender.amount - amount;
        let remaining: Token = Token {
            owner: sender.owner,
            amount: difference,
        };
        return (remaining, final {
            let current: u64 = Mapping::get_or_use(account, receiver, 0u64);
            Mapping::set(account, receiver, current + amount);
        });
    }

    // Convert public tokens to private: debits the caller's public `account`
    // balance on-chain and produces a new Token record for the receiver.
    fn public_to_private(public receiver: address, public amount: u64) -> (Token, Final) {
        let transferred: Token = Token {
            owner: receiver,
            amount: amount,
        };
        let caller = self.caller;
        return (transferred, final {
            let current: u64 = Mapping::get_or_use(account, caller, 0u64);
            Mapping::set(account, caller, current - amount);
        });
    }
}
