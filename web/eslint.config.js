import js from '@eslint/js';
import tseslint from 'typescript-eslint';
import svelte from 'eslint-plugin-svelte';
import globals from 'globals';

export default tseslint.config(
  js.configs.recommended,
  ...tseslint.configs.strictTypeChecked,
  ...svelte.configs['flat/recommended'],
  {
    languageOptions: {
      globals: { ...globals.browser },
      parserOptions: {
        projectService: true,
        extraFileExtensions: ['.svelte'],
        tsconfigRootDir: import.meta.dirname
      }
    }
  },
  {
    files: ['**/*.svelte', '**/*.svelte.ts'],
    languageOptions: {
      parserOptions: { parser: tseslint.parser }
    }
  },
  {
    // `$bindable()` is a rune, not a default value.
    files: ['**/*.svelte'],
    rules: { '@typescript-eslint/no-useless-default-assignment': 'off' }
  },
  {
    files: ['scripts/**/*.mjs', 'e2e/**/*.ts'],
    languageOptions: {
      globals: { ...globals.node }
    }
  },
  {
    rules: {
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/restrict-template-expressions': ['error', { allowNumber: true }],
      '@typescript-eslint/no-confusing-void-expression': ['error', { ignoreArrowShorthand: true }],
      '@typescript-eslint/no-unnecessary-condition': ['error', { allowConstantLoopConditions: 'only-allowed-literals' }],
      'no-console': 'off'
    }
  },
  {
    // Mock mode lives behind the API boundary: only main.ts may choose it.
    files: ['src/**'],
    ignores: ['src/mock/**', 'src/main.ts'],
    rules: {
      'no-restricted-syntax': [
        'error',
        { selector: 'Identifier[name=/^(isMock|VITE_MOCK)$/]', message: 'Only src/mock and main.ts know about mock mode.' },
        { selector: 'Literal[value="VITE_MOCK"]', message: 'Only src/mock and main.ts know about mock mode.' }
      ],
      'no-restricted-imports': [
        'error',
        { patterns: [{ group: ['**/mock', '**/mock/**'], message: 'The app reaches the mock only through the api object.' }] }
      ]
    }
  },
  {
    // Build tooling outside every tsconfig: linted without type information.
    files: ['*.config.js', '*.config.ts', 'scripts/**/*.mjs'],
    ...tseslint.configs.disableTypeChecked
  },
  {
    ignores: ['dist/**', 'e2e/out/**', 'playwright-report/**']
  }
);
