// Test file for the {{PROJECT_NAME}} payment program.
// Tests: transfer returns the amount unchanged for a valid input.

import {{PROJECT_NAME}}.aleo;

program test_program.aleo {
    @noupgrade
    constructor() {}

    @test
    fn test_transfer() {
        let result = {{PROJECT_NAME}}.aleo::transfer(100u64);
        assert_eq(result, 100u64);
    }
}
