RSpec.describe Widget do
  it do
    Widget.new.ready?
    expect(Widget.new).to be_ready
  end
end
