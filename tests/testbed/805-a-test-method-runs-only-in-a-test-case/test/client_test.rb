class FakeClient
  def test_connection
  end
end

class ClientTest < Minitest::Test
  def test_one
    FakeClient.new
  end
end
