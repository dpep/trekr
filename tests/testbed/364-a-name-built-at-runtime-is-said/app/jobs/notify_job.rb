class NotifyJob
  def perform(type, user)
    Notifier.public_send(type, user)
  end
end
