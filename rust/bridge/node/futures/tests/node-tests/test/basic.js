//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

const { assert } = require('chai');

const native = require(process.env.SIGNAL_NEON_FUTURES_TEST_LIB);

function promisify(operation) {
  return function() {
    return new Promise((resolve, reject) => {
      try {
        operation(...arguments, resolve);
      } catch (error) {
        reject(error);
      }
    });
  };
}

describe('native', () => {
  it('can fulfill promises', async () => {
    const result = await promisify(native.incrementAsync)(Promise.resolve(5));
    assert.equal(result, 6);
  });

  it('can handle rejection', async () => {
    const result = await promisify(native.incrementAsync)(
      Promise.reject('badness'),
    );
    assert.equal(result, 'error: badness');
  });
});
