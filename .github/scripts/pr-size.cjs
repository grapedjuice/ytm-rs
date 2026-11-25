// Gives a pull request one size label from the lines it adds and removes.
// Run by .github/workflows/pr-labels.yml through actions/github-script.

const SIZES = [
  ['size: XS', 10],
  ['size: S', 50],
  ['size: M', 200],
  ['size: L', 500],
  ['size: XL', 1000],
  ['size: XXL', Infinity],
];

// Lines nobody reviews by hand: lockfiles, translations synced from Crowdin
// (en-US is written by hand, so it counts), and the site's vendored scripts.
function skipped(path) {
  return (
    /(^|\/)(Cargo|flake)\.lock$/.test(path) ||
    (path.startsWith('locales/') && !path.startsWith('locales/en-US/')) ||
    path.startsWith('site/vendor/')
  );
}

module.exports = async ({ github, context }) => {
  const repo = context.repo;
  const number = context.payload.pull_request.number;

  const files = await github.paginate(github.rest.pulls.listFiles, { ...repo, pull_number: number, per_page: 100 });
  const lines = files
    .filter(({ filename }) => !skipped(filename))
    .reduce((sum, { additions, deletions }) => sum + additions + deletions, 0);
  const [wanted] = SIZES.find(([, maximum]) => lines <= maximum);

  const issue = { ...repo, issue_number: number };
  const { data: labels } = await github.rest.issues.listLabelsOnIssue(issue);
  const current = new Set(labels.map(({ name }) => name));
  for (const [name] of SIZES) {
    if (name !== wanted && current.has(name)) {
      await github.rest.issues.removeLabel({ ...issue, name });
    }
  }
  if (!current.has(wanted)) {
    await github.rest.issues.addLabels({ ...issue, labels: [wanted] });
  }
};
