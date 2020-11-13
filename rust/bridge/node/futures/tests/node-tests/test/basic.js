//
// Copyright 2020 Signal Messenger, LLC.
// SPDX-License-Identifier: AGPL-3.0-only
//

const chai = require('chai');
const chaiAsPromised = require('chai-as-promised');  

const { assert, expect } = chai;  
chai.use(chaiAsPromised);  

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

  it('promises can fulfill promises', async () => {
    const result = await native.incrementPromise(Promise.resolve(5));
    assert.equal(result, 6);
  });

  it('promises can handle rejection', () => {
    const promise = native.incrementPromise(
      Promise.reject('badness'),
    )
    return assert.isRejected(promise, /badness/);
  });

  describe('recovery', () => {
    it('handles pre-await panics', () => {
      const promise = native.panicPreAwait(
        Promise.resolve(6),
      );
      return assert.isRejected(promise, /unexpected panic: check for this/);
      
    })

    it('handles callback panics', () => {
      const promise = native.panicDuringCallback(
        Promise.resolve(6),
      );
      return assert.isRejected(promise, /unexpected panic: check for this/);
      
    })

    it('handles post-await panics', () => {
      const promise = native.panicPostAwait(
        Promise.resolve(6),
      );
      return assert.isRejected(promise, /unexpected panic: check for this/);
      
    })

    it('handles fulfillment panics', () => {
      const promise = native.panicDuringFulfill(
        Promise.resolve(6),
      );
      return assert.isRejected(promise, /unexpected panic: check for this/);
      
    })
  });
});
