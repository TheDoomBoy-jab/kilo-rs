import unittest
from scripts.profile_flames import flame_svg, parse_sample


class FlameTests(unittest.TestCase):
    def test_inclusive_sample_counts_become_exclusive_stacks(self):
        text = '''Header\nCall graph:
    10 Thread_1
      10 main  (in editor) + 4  [0x100]
      + 6 render  (in editor) + 4  [0x104]
      + ! 4 write  (in kernel) + 4  [0x108]
      + 3 read  (in kernel) + 4  [0x10c]

Sort by top of stack:
        4 write
'''
        folded = parse_sample(text)
        self.assertEqual(sum(folded.values()), 10)
        self.assertEqual(folded[('[main thread]', 'main', 'render')], 2)
        self.assertEqual(folded[('[main thread]', 'main', 'render', 'write')], 4)
        self.assertEqual(folded[('[main thread]', 'main')], 1)
        svg, height = flame_svg(folded, 'C & Rust', 8, 1)
        self.assertIn('C &amp; Rust', svg)
        self.assertIn('including terminal I/O waits', svg)
        self.assertGreater(height, 200)

    def test_rejects_inconsistent_counts(self):
        with self.assertRaises(ValueError):
            parse_sample('Call graph:\n    1 parent\n      2 child\n')
