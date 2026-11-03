RSpec.describe "store" do
  let(:store) { Store.new }
  let(:shelf) { Shelf.new }

  it do
    expect(store).to be_exist
    expect(shelf).to be_exist
    expect(thing).to be_exist
  end
end
