class Secret
  def go = public_send(:hidden)

  private

  def hidden = 1
end
Secret.new.go
