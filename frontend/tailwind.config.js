/** @type {import('tailwindcss').Config} */
module.exports = {
  darkMode: 'class',
  content: [
    './index.html',
    './src/**/*.rs',
  ],
  theme: {
    extend: {
      colors: {
        pyra: {
          orange: '#F97316',
          orangeDark: '#EA580C',
          cyan: '#06B6D4',
          sky: '#0284C7',
          darkBg: '#14171A',
          darkSurface: '#1E2227',
          darkSidebar: '#111315',
          darkBorder: '#2A2F37',
          darkText: '#F1F5F9',
          lightBg: '#F8FAFC',
          lightSidebar: '#F1F5F9',
          lightBorder: '#E2E8F0',
          lightText: '#0F172A',
        },
      },
    },
  },
  plugins: [],
};

