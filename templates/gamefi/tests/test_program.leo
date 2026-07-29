// Test file for the {{PROJECT_NAME}} GameFi program.
// Tests: submit_score returns a record with the expected score field.

import {{PROJECT_NAME}}.aleo;

program test_program.aleo {
    @noupgrade
    constructor() {}

    @test
    fn test_submit_score() {
        let result = {{PROJECT_NAME}}.aleo::submit_score(
            aleo172leh6aw467q7jzer533wcj8dzzsat3kr446g3nys3pmgqljcs8sfea3eh,
            100u64,
            5u64,
            12345u64,
        );
        assert_eq(result.score, 100u64);
    }
}
