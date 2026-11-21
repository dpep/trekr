RSpec.describe OwnSubject do
  it { expect(subject.own_value).to eq 2 }

  it "builds" do
    expect(described_class.new.own_value).to eq 2
  end
end
