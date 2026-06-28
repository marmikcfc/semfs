"""Generate the synthetic E11 discovery-stressed corpus.

Creates two subdirectories with 200 files each:
  data/   -- product quarterly reports (for e11-001)
  reports/ -- regional sales summaries (for e11-002)

The "needle" file in each set has the unique correct answer; all other files
have plausible-but-wrong data that would mislead an agent that guesses or scans
without search.

Usage:
  python3 generate_e11_corpus.py <output_dir>
  # Then upload to Modal: modal volume put semfs-bench-data <output_dir>/. /data/corpus/e11_discovery_corpus/
"""

import os
import random
import sys

random.seed(42)

PRODUCTS = [
    "Widget-Alpha", "Widget-Beta", "Widget-Gamma", "Widget-Delta",
    "Widget-Epsilon", "Widget-Zeta", "Widget-Eta", "Widget-Theta",
    "Widget-Iota", "Widget-Kappa", "Widget-Lambda", "Widget-Mu",
    "Widget-Nu", "Widget-Xi", "Widget-Omicron", "Widget-Pi",
    "Widget-Rho", "Widget-Sigma", "Widget-Tau", "Widget-Upsilon",
    "Widget-Phi", "Widget-Chi", "Widget-Psi", "Widget-Omega",
]

REGIONS = [
    "North America", "South America", "Western Europe", "Eastern Europe",
    "Middle East", "Africa", "South Asia", "East Asia", "Southeast Asia",
    "Oceania", "Central Asia", "Caribbean",
]

QUARTERS = ["Q1", "Q2", "Q3", "Q4"]
YEARS = [2022, 2023, 2024]


def gen_return_rate(product_idx: int, year: int, quarter: str) -> float:
    """Generate a return rate. product 041 (index) in Q4-2023 gets 8.73%."""
    r = random.uniform(0.5, 6.0)
    # needle: file 042 (1-indexed) = index 41, Q4-2023
    if product_idx == 41 and year == 2023 and quarter == "Q4":
        r = 8.73
    return round(r, 2)


def gen_growth(region_idx: int, year: int, half: str) -> float:
    """Generate a growth rate. region 072 (index) in H1-2024 gets 34.6%."""
    r = random.uniform(-5.0, 28.0)
    # needle: file 073 (1-indexed) = index 72, H1-2024
    if region_idx == 72 and year == 2024 and half == "H1":
        r = 34.6
    return round(r, 1)


def write_product_reports(base_dir: str) -> None:
    data_dir = os.path.join(base_dir, "data")
    os.makedirs(data_dir, exist_ok=True)
    for i in range(200):
        product = PRODUCTS[i % len(PRODUCTS)]
        fname = f"product_report_{i+1:03d}.txt"
        lines = [
            f"Product Quarterly Report",
            f"Product: {product} (variant {i+1})",
            f"Report generated: 2024-Q1",
            "",
        ]
        for year in YEARS:
            for q in QUARTERS:
                rr = gen_return_rate(i, year, q)
                sales = random.randint(500, 50000)
                units = random.randint(100, 10000)
                lines.append(
                    f"Period: {year}-{q} | units_sold: {units} | revenue: ${sales} | return_rate: {rr}%"
                )
        lines.append("")
        lines.append("End of report.")
        with open(os.path.join(data_dir, fname), "w") as f:
            f.write("\n".join(lines))
    print(f"Wrote {i+1} product report files to {data_dir}/")


def write_region_summaries(base_dir: str) -> None:
    reports_dir = os.path.join(base_dir, "reports")
    os.makedirs(reports_dir, exist_ok=True)
    for i in range(200):
        region = REGIONS[i % len(REGIONS)]
        fname = f"region_summary_{i+1:03d}.txt"
        lines = [
            f"Regional Sales Summary",
            f"Region: {region} (territory {i+1})",
            f"Report date: 2024-07-01",
            "",
        ]
        for year in YEARS:
            for half in ["H1", "H2"]:
                growth = gen_growth(i, year, half)
                revenue = random.randint(100000, 5000000)
                lines.append(
                    f"Period: {year}-{half} | revenue: ${revenue} | yoy_growth: {growth}%"
                )
        lines.append("")
        lines.append("End of summary.")
        with open(os.path.join(reports_dir, fname), "w") as f:
            f.write("\n".join(lines))
    print(f"Wrote {i+1} region summary files to {reports_dir}/")


def main():
    if len(sys.argv) < 2:
        print(f"Usage: {sys.argv[0]} <output_dir>")
        sys.exit(1)
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    write_product_reports(out)
    write_region_summaries(out)
    print(f"\nCorpus ready at {out}/")
    print("Needle for e11-001: data/product_report_042.txt, Widget-Sigma (variant 42), Q4-2023, return_rate=8.73%")
    print("Needle for e11-002: reports/region_summary_073.txt, North America (territory 73), H1-2024, growth=34.6%")


if __name__ == "__main__":
    main()
