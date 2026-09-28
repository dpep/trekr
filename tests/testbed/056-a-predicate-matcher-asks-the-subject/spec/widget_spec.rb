RSpec.describe Widget do
  it do
    widget = Widget.new
    expect(widget).to be_empty
    expect(widget).not_to have_key(:a)
    expect(widget).to be_exist
    expect(widget).to be_within(1)
    expect(thing).to be_empty
    expect(widget).to be_full
    expect([widget]).to all(be_empty)
  end
end
