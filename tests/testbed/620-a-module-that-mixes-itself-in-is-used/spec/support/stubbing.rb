module Stubbing
  def run
    :stubbed
  end

  Widget.prepend self
end
