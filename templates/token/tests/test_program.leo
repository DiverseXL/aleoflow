// Test file for the {{PROJECT_NAME}} token program.
// Tests: mint creates a Token record, and transfer correctly splits one.

import {{PROJECT_NAME}}.aleo;

program test_program.aleo {
    @noupgrade
    constructor() {}

    @test
    fn test_mint() {
        let token = {{PROJECT_NAME}}.aleo::mint(self.caller, 100u64);
        assert_eq(token.amount, 100u64);
        assert_eq(token.owner, self.caller);
    }

    @test
    fn test_transfer() {
        // Mint a token to the test signer, then transfer part of it to
        // a distinct receiver address.
        let token = {{PROJECT_NAME}}.aleo::mint(self.caller, 100u64);
        let receiver = aleo172leh6aw467q7jzer533wcj8dzzsat3kr446g3nys3pmgqljcs8sfea3eh;
        let (remaining, transferred) = {{PROJECT_NAME}}.aleo::transfer(
            token,
            receiver,
            30u64,
        );
        assert_eq(remaining.amount, 70u64);
        assert_eq(remaining.owner, self.caller);
        assert_eq(transferred.amount, 30u64);
        assert_eq(transferred.owner, receiver);
    }
}
