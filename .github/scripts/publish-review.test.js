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

test('creates the first sticky comment', async () => {
  const mock = harness([]);
  await publishReview({ ...mock, body: `${MARKER}\n**Тесты:** \`passed\`` });
  assert.equal(mock.calls.filter(([kind]) => kind === 'create').length, 1);
  assert.equal(mock.calls.filter(([kind]) => kind === 'update').length, 0);
});

test('updates one bot sticky comment without creating duplicates', async () => {
  const mock = harness([
    { id: 10, user: { login: 'human' }, body: MARKER },
    { id: 11, user: { login: 'github-actions[bot]' }, body: MARKER },
    { id: 12, user: { login: 'github-actions[bot]' }, body: MARKER },
  ]);
  await publishReview({ ...mock, body: `${MARKER}\n**Тесты:** \`failed\`` });
  const updates = mock.calls.filter(([kind]) => kind === 'update');
  assert.equal(updates.length, 1);
  assert.equal(updates[0][1].comment_id, 11);
  assert.equal(mock.calls.filter(([kind]) => kind === 'create').length, 0);
  assert.ok(mock.calls.some(([kind, value]) => kind === 'summary' && value.includes('failed')));
});
