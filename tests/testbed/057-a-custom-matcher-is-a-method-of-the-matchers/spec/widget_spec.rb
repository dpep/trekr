RSpec.describe Widget do
  matcher :be_shiny do
    match { |actual| actual }
  end

  it do
    expect(1).to have_widget(1)
    expect([]).to exclude(1)
    expect(2).to be_shiny
  end

  it { expect(3).to be_dull }
end
