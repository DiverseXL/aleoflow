program diag_test.aleo;

function transfer:
    input r0 as u64.private;
    assert.neq r0 0u64;
    output r0 as u64.private;

constructor:
    assert.eq edition 0u16;
