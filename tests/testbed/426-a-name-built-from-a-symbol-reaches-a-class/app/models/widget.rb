class Widget
  def notify
    Jobs.enqueue(:send_digest)
  end
end
