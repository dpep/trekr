module Naming
  def to_param
  end
end

Post = Struct.new(:title, :body) do
  include Naming
end
