const MARKER = '<!-- fox-llm-ai-review -->';

async function publishReview({ github, context, core, body }) {
  if (!body.includes(MARKER)) throw new Error('review marker is missing');
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
  const status = body.match(/\*\*Тесты:\*\* `([^`]+)`/)?.[1] || 'unknown';
  await core.summary
    .addHeading('AI review')
    .addRaw('Tests: ' + status + '<br>Sticky comment updated.')
    .write();
  return existing ? 'updated' : 'created';
}

module.exports = { MARKER, publishReview };
