const test = require('node:test');
const assert = require('node:assert/strict');
const { publishReview, MARKER } = require('./publish-review');

function harness(comments) {
  const calls = [];
  const github = {
    paginate: async () => comments,
    rest: { issues: {
      listComments: async () => {},
      createComment: async (value) => calls.push(['create', value]),
      updateComment: async (value) => calls.push(['update', value]),
    } },
  };
  const summary = { addHeading() { return this; }, addRaw(value) { calls.push(['summary', value]); return this; }, async write() {} };
  return { github, core: { summary }, context: { repo: { owner: 'acme', repo: 'demo' }, issue: { number: 7 } }, calls };
}

function result(retrieval_mode = 'embedding') {
  return {
    retrieval_mode,
    cached_embeddings: 12,
    new_embeddings: 3,
    test_report: { status: 'passed' },
  };
}

test('creates the first sticky comment', async () => {
  const mock = harness([]);
  await publishReview({ ...mock, body: MARKER, reviewResult: result() });
  assert.equal(mock.calls.filter(([kind]) => kind === 'create').length, 1);
  assert.equal(mock.calls.filter(([kind]) => kind === 'update').length, 0);
});

test('updates one bot sticky comment without creating duplicates', async () => {
  const mock = harness([
    { id: 10, user: { login: 'human' }, body: MARKER },
    { id: 11, user: { login: 'github-actions[bot]' }, body: MARKER },
    { id: 12, user: { login: 'github-actions[bot]' }, body: MARKER },
  ]);
  const reviewResult = result('lexical_fallback');
  reviewResult.test_report.status = 'failed';
  await publishReview({ ...mock, body: MARKER, reviewResult });
  const updates = mock.calls.filter(([kind]) => kind === 'update');
  assert.equal(updates.length, 1);
  assert.equal(updates[0][1].comment_id, 11);
  assert.equal(mock.calls.filter(([kind]) => kind === 'create').length, 0);
  assert.ok(mock.calls.some(([kind, value]) => kind === 'summary' && value.includes('failed')));
  assert.ok(mock.calls.some(([kind, value]) => kind === 'summary' && value.includes('lexical_fallback')));
  assert.ok(mock.calls.some(([kind, value]) => kind === 'summary' && value.includes('12')));
});

test('rejects unknown retrieval mode and invalid counters before publishing', async () => {
  const mock = harness([]);
  await assert.rejects(
    publishReview({ ...mock, body: MARKER, reviewResult: result('unknown') }),
    /retrieval_mode/,
  );
  const invalid = result();
  invalid.new_embeddings = -1;
  await assert.rejects(
    publishReview({ ...mock, body: MARKER, reviewResult: invalid }),
    /new_embeddings/,
  );
  assert.equal(mock.calls.length, 0);
});
