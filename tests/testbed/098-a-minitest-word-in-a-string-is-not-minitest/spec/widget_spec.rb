describe Widget do
  # Reported as "widget.save.must_be_valid" when it fails.
  it { expect("widget.save.must_be_valid").to eq "x.wont_be" }
end
