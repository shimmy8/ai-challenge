const MARKER = '<!-- fox-llm-ai-review -->';

async function publishReview({ github, context, core, body, reviewResult }) {
  if (!body.includes(MARKER)) throw new Error('review marker is missing');
  const validModes = new Set(['embedding', 'lexical_fallback']);
  if (!reviewResult || !validModes.has(reviewResult.retrieval_mode)) {
    throw new Error('review JSON has invalid retrieval_mode');
  }
  for (const field of ['cached_embeddings', 'new_embeddings']) {
    if (!Number.isSafeInteger(reviewResult[field]) || reviewResult[field] < 0) {
      throw new Error(`review JSON has invalid ${field}`);
    }
  }
  const { owner, repo } = context.repo;
  const issue_number = context.issue.number;
  const comments = await github.paginate(github.rest.issues.listComments, {
    owner, repo, issue_number, per_page: 100,
  });
  const existing = comments.find(comment =>
    comment.user?.login === 'github-actions[bot]' && comment.body?.includes(MARKER));
  if (existing) {
    await github.rest.issues.updateComment({ owner, repo, comment_id: existing.id, body });
  } else {
    await github.rest.issues.createComment({ owner, repo, issue_number, body });
  }
  const status = reviewResult.test_report?.status || 'unknown';
  await core.summary
    .addHeading('AI review')
    .addRaw(
      'Tests: ' + status +
      '<br>Retrieval: ' + reviewResult.retrieval_mode +
      '<br>Cached embeddings: ' + reviewResult.cached_embeddings +
      '<br>New embeddings: ' + reviewResult.new_embeddings +
      '<br>Sticky comment updated.'
    )
    .write();
  return existing ? 'updated' : 'created';
}

module.exports = { MARKER, publishReview };
