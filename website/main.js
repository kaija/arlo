/**
 * Arlo Website — Main JavaScript
 * Handles mobile nav toggle and smooth scroll behavior.
 */

(function () {
  'use strict';

  // Mobile navigation toggle
  const navToggle = document.querySelector('.nav-toggle');
  const navLinks = document.querySelector('.nav-links');

  if (navToggle && navLinks) {
    navToggle.addEventListener('click', function () {
      const isOpen = navLinks.classList.toggle('open');
      navToggle.setAttribute('aria-expanded', String(isOpen));
    });

    // Close nav on link click (mobile)
    navLinks.querySelectorAll('a').forEach(function (link) {
      link.addEventListener('click', function () {
        navLinks.classList.remove('open');
        navToggle.setAttribute('aria-expanded', 'false');
      });
    });
  }

  // Close mobile nav on outside click
  document.addEventListener('click', function (e) {
    if (navLinks && navLinks.classList.contains('open')) {
      if (!navLinks.contains(e.target) && !navToggle.contains(e.target)) {
        navLinks.classList.remove('open');
        navToggle.setAttribute('aria-expanded', 'false');
      }
    }
  });

  // Add subtle nav background on scroll
  const nav = document.querySelector('.nav');
  if (nav) {
    window.addEventListener('scroll', function () {
      if (window.scrollY > 10) {
        nav.style.borderBottomColor = 'rgba(0, 0, 0, 0.08)';
      } else {
        nav.style.borderBottomColor = 'rgba(0, 0, 0, 0.06)';
      }
    }, { passive: true });
  }
})();
