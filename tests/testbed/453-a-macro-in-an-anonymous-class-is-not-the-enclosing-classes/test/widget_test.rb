class WidgetTest < Fixtures
  def test_anonymous
    Class.new(ActiveRecord::Base) do
      has_many :parts
      attr_reader :size
    end
  end

  def test_fixture
    parts(:one).find(1)
    size
  end
end
