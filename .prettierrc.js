/** @type {import("prettier").Config} */
export default {
  bracketSpacing: true,
  bracketSameLine: true,
  singleQuote: true,
  trailingComma: 'all',
  arrowParens: 'avoid',
  semi: true,
  plugins: ['prettier-plugin-organize-imports'],
  overrides: [
    {
      files: ['*.ts', '*.js', '*.tsx', '*.jsx', '*.cjs', '*.mjs'],
      options: {
        printWidth: 120,
      },
    }
  ],
};